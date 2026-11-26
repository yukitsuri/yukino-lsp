use std::path::Path;
use tower_lsp::lsp_types::{
    Command, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    InsertTextFormat, Position, TextEdit,
};
use yukic_ast::{ImportPath, ImportPattern, ItemKind, Module, PathPrefix};
use yukic_span::BytePos;

use crate::document::Document;
use crate::workspace::WorkspaceEnv;

pub fn compute_completions(
    doc: &Document,
    pos: Position,
    module: &Module,
    env: &WorkspaceEnv,
) -> CompletionResponse {
    let mut items = Vec::new();
    let offset = doc.position_to_byte_offset(pos);
    let prefix = get_prefix_before_offset(&doc.text, offset);
    let current_file = doc.uri.to_file_path().unwrap_or_default();

    // `cload "<cursor>"` / `cload "<cursor>` (and `load "<cursor>`) header path completions
    if let Some(cload_ctx) = extract_cload_path_context(&doc.text, offset) {
        let range = doc.span_to_range(
            BytePos(u32::try_from(cload_ctx.path_start_byte).unwrap_or(0)),
            BytePos(u32::try_from(offset).unwrap_or(0)),
        );
        for rel_path in env.list_header_files(&current_file) {
            if !cload_ctx.partial.is_empty() && !rel_path.contains(cload_ctx.partial) {
                continue;
            }
            let new_text = if cload_ctx.has_closing_quote {
                rel_path.clone()
            } else {
                format!("{rel_path}\"")
            };
            items.push(CompletionItem {
                label: rel_path.clone(),
                kind: Some(CompletionItemKind::FILE),
                detail: Some("C header".to_string()),
                filter_text: Some(rel_path),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text })),
                insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
                ..Default::default()
            });
        }
        return CompletionResponse::Array(items);
    }

    let current_fun = find_enclosing_fun(module, offset, &doc.text);
    let current_class = find_enclosing_class(module, offset, &doc.text);

    // suppress suggs when user is naming a new entity
    if is_identifier_naming_position(prefix) {
        return CompletionResponse::Array(Vec::new());
    }

    // extends kw right after class name: class Hyi
    if is_after_class_name(prefix) {
        items.push(CompletionItem {
            label: "extends".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Inherit from another class".to_string()),
            insert_text: Some("extends ".to_string()),
            insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
            command: Some(Command {
                title: "triggerSuggest".to_string(),
                command: "editor.action.triggerSuggest".to_string(),
                arguments: None,
            }),
            ..Default::default()
        });
        return CompletionResponse::Array(items);
    }

    // inside extends clause: suggest ONLY non-locked classes
    if is_extends_position(prefix) {
        let current_decl = extract_current_decl_name(prefix);
        populate_non_locked_classes(&mut items, module, &current_file, env, current_decl.as_deref());
        return CompletionResponse::Array(items);
    }

    // after init keyword: suggest ONLY available classes
    if is_init_position(prefix) {
        populate_available_classes(&mut items, module, &current_file, env, current_class.as_deref());
        return CompletionResponse::Array(items);
    }

    // after rewrite keyword: suggest ONLY mutable variables
    if is_rewrite_target_position(prefix) {
        populate_rewrite_targets(&mut items, module, offset, &doc.text);
        dedup_completion_items(&mut items);
        return CompletionResponse::Array(items);
    }

    // load statements (package, submodule, or symbols)
    if let Some(ctx) = extract_load_context(prefix) {
        match ctx {
            LoadContext::PackageRoot(_) => {
                for lib in &env.available_libs {
                    items.push(CompletionItem {
                        label: lib.clone(),
                        kind: Some(CompletionItemKind::CLASS),
                        detail: Some(format!("Library: {lib}")),
                        ..Default::default()
                    });
                }
                items.push(CompletionItem {
                    label: ".".to_string(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some("Current folder module".to_string()),
                    ..Default::default()
                });
                items.push(CompletionItem {
                    label: "..".to_string(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some("Parent folder module".to_string()),
                    ..Default::default()
                });
                items.push(CompletionItem {
                    label: "root".to_string(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some("Project root module".to_string()),
                    ..Default::default()
                });
                return CompletionResponse::Array(items);
            }
            LoadContext::Submodule { pkg, .. } => {
                let items = resolve_submodule_completions(pkg, &current_file, env);
                return CompletionResponse::Array(items);
            }
            LoadContext::Symbols { module_path, partial } => {
                let last_token = partial.split(',').last().unwrap_or("").trim();
                if last_token.contains(" as ") || last_token.ends_with(" as") || last_token.starts_with("as ") || last_token == "as" {
                    return CompletionResponse::Array(vec![]);
                }
                populate_import_symbols(&mut items, module_path, &current_file, env);
                return CompletionResponse::Array(items);
            }
        }
    }

    // scoped calls like: std:: or std::io:: or header_stem::
    if let Some((segments, partial)) = extract_scoped_path(prefix) {
        populate_scoped_completions(&mut items, &segments, partial, module, env, &current_file);
        return CompletionResponse::Array(items);
    }

    // dot member access: instance.<cursor>
    if let Some((receiver, _partial)) = extract_dot_receiver(prefix) {
        populate_instance_dot_completions(
            &mut items,
            receiver,
            module,
            &current_file,
            env,
            &doc.text,
        );
        return CompletionResponse::Array(items);
    }

    // extends context: suggest ONLY non-locked classes
    if is_extends_position(prefix) {
        let current_decl = extract_current_decl_name(prefix);
        populate_non_locked_classes(&mut items, module, &current_file, env, current_decl.as_deref());
        return CompletionResponse::Array(items);
    }

    // type annotation context: `: <type>`, `-> <type>`, `as <type>`
    if is_type_annotation_position(prefix) {
        populate_types(&mut items);
        populate_type_symbols(&mut items, module, &current_file);
        return CompletionResponse::Array(items);
    }

    // inside compiler attributes: `#[...]`
    if is_inside_attribute(prefix) {
        populate_compiler_attributes(&mut items, prefix);
        return CompletionResponse::Array(items);
    }

    // inside C ABI attributes: `|...|`
    if is_inside_c_attribute(prefix) {
        populate_c_attributes(&mut items);
        return CompletionResponse::Array(items);
    }

    // general expression / statement context
    populate_keywords(&mut items, prefix);
    populate_types(&mut items);
    populate_intrinsics(&mut items);
    populate_attribute_snippets(&mut items);
    populate_in_scope_variables(&mut items, offset, &doc.text, module, current_fun.as_deref(), current_class.as_deref());
    populate_ast_symbols(&mut items, module, prefix, current_fun.as_deref(), current_class.as_deref());
    populate_cload_symbols(&mut items, module, &current_file, env);
    populate_imported_symbols(&mut items, module, &current_file, env, prefix, current_fun.as_deref(), current_class.as_deref());
    if let Some(parent) = current_file.parent() {
        for cls in crate::workspace::scan_spes_classes(parent) {
            if current_class.as_deref() == Some(&cls) {
                continue;
            }
            let (snippet, fmt) = class_snippet(&cls, prefix);
            items.push(CompletionItem {
                label: cls.clone(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some(format!("class {cls} (spes)")),
                insert_text: Some(snippet),
                insert_text_format: fmt,
                ..Default::default()
            });
        }
    }

    dedup_completion_items(&mut items);
    CompletionResponse::Array(items)
}

fn is_identifier_naming_position(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }

    let last = *words.last().unwrap();
    let second_to_last = words.iter().rev().nth(1).copied();

    // immediately after decl kw
    let decl_keywords = [
        "fun", "cfun", "class", "struct", "cstruct", "cunion", "cvar", "interface", "enum", "type", "define",
    ];
    if decl_keywords.contains(&last) && prefix.ends_with(' ') {
        return true;
    }

    // typing a new name right after a decl kw
    if let Some(prev) = second_to_last {
        if decl_keywords.contains(&prev) && !last.contains('(') && !last.contains('{') && !last.contains(':') {
            if prefix.ends_with(' ') {
                return false;
            }
            return true;
        }
    }

    // `set <name>` before `:` or `=`
    if last == "set" && prefix.ends_with(' ') {
        return true;
    }
    if let Some("set") = second_to_last {
        if !last.contains(':') && !last.contains('=') {
            return true;
        }
    }

    // `as <alias>` in load clauses or aliases
    if last == "as" && prefix.ends_with(' ') {
        return true;
    }
    if let Some("as") = second_to_last {
        if !last.contains(',') && !last.contains(';') && !last.contains('}') && !last.contains('\n') {
            if !prefix.ends_with(' ') {
                return true;
            }
        }
    }

    false
}

fn is_rewrite_target_position(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }
    let last = *words.last().unwrap();
    let second_to_last = words.iter().rev().nth(1).copied();

    if last == "rewrite" && prefix.ends_with(' ') {
        return true;
    }
    if let Some("rewrite") = second_to_last {
        if !last.contains('=') && !last.contains(';') && !last.contains('.') && !last.contains("::") {
            return true;
        }
    }
    false
}

fn is_in_class_scope(module: &Module, offset: usize, doc_text: &str) -> bool {
    for item in &module.items {
        if let ItemKind::Class(_) = item.kind {
            if (item.span.start.0 as usize) <= offset && offset <= (item.span.end.0 as usize) {
                return true;
            }
        }
    }
    find_enclosing_class_context(offset, doc_text).is_some()
}

fn find_enclosing_class_context(offset: usize, doc_text: &str) -> Option<String> {
    let before = &doc_text[..offset.min(doc_text.len())];
    let mut balance: i32 = 0;
    for line in before.lines().rev() {
        let trimmed = line.trim();
        for ch in trimmed.chars().rev() {
            if ch == '}' {
                balance -= 1;
            } else if ch == '{' {
                if balance < 0 {
                    balance += 1;
                } else {
                    for kw in &["class ", "public class ", "locked class ", "public locked class "] {
                        if let Some(rest) = trimmed.strip_prefix(kw) {
                            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                            if !name.is_empty() {
                                return Some(name);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn find_enclosing_class(module: &Module, offset: usize, doc_text: &str) -> Option<String> {
    for item in &module.items {
        if let ItemKind::Class(ref c) = item.kind {
            if (item.span.start.0 as usize) <= offset && offset <= (item.span.end.0 as usize) {
                return Some(c.name.name.clone());
            }
        }
    }
    find_enclosing_class_context(offset, doc_text)
}

fn find_enclosing_fun(module: &Module, offset: usize, doc_text: &str) -> Option<String> {
    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) => {
                if (item.span.start.0 as usize) <= offset && offset <= (item.span.end.0 as usize) {
                    return Some(f.name.name.clone());
                }
            }
            ItemKind::Class(ref c) => {
                for m in &c.methods {
                    if (m.span.start.0 as usize) <= offset && offset <= (m.span.end.0 as usize) {
                        return Some(m.name.name.clone());
                    }
                }
            }
            _ => {}
        }
    }
    find_enclosing_fun_context(offset, doc_text)
}

fn find_enclosing_fun_context(offset: usize, doc_text: &str) -> Option<String> {
    let before = &doc_text[..offset.min(doc_text.len())];
    let mut balance: i32 = 0;
    for line in before.lines().rev() {
        let trimmed = line.trim();
        for ch in trimmed.chars().rev() {
            if ch == '}' {
                balance -= 1;
            } else if ch == '{' {
                if balance < 0 {
                    balance += 1;
                } else {
                    for kw in &["fun ", "public fun ", "cfun ", "public cfun "] {
                        if let Some(rest) = trimmed.strip_prefix(kw) {
                            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                            if !name.is_empty() {
                                return Some(name);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn populate_class_rewrite_fields_from_text(
    items: &mut Vec<CompletionItem>,
    cls_name: &str,
    doc_text: &str,
    seen: &mut std::collections::HashSet<String>,
) {
    let mut in_cls = false;
    let mut depth: i32 = 0;
    for line in doc_text.lines() {
        let trimmed = line.trim();
        if !in_cls {
            for kw in &["class ", "public class ", "locked class ", "public locked class "] {
                if let Some(rest) = trimmed.strip_prefix(kw) {
                    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    if name == cls_name {
                        in_cls = true;
                        break;
                    }
                }
            }
        }
        if in_cls {
            for ch in trimmed.chars() {
                if ch == '{' { depth += 1; }
                else if ch == '}' {
                    depth -= 1;
                    if depth <= 0 { in_cls = false; break; }
                }
            }
            if depth == 1 && !trimmed.starts_with("fun ") && !trimmed.starts_with("public fun ") && !trimmed.starts_with("cfun ") {
                let rest = trimmed.strip_prefix("public ").unwrap_or(trimmed);
                let rest = rest.strip_prefix("set ").unwrap_or(rest);
                if let Some(colon_pos) = rest.find(':') {
                    let field_name: String = rest[..colon_pos].trim().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    if !field_name.is_empty() && field_name != "class" {
                        let this_field = format!("this.{field_name}");
                        if seen.insert(this_field.clone()) {
                            items.push(CompletionItem {
                                label: this_field,
                                kind: Some(CompletionItemKind::FIELD),
                                detail: Some(format!("field {field_name}")),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }
}

fn populate_rewrite_targets(
    items: &mut Vec<CompletionItem>,
    module: &Module,
    offset: usize,
    doc_text: &str,
) {
    let mut seen = std::collections::HashSet::new();
    let in_class = is_in_class_scope(module, offset, doc_text);

    for line in doc_text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("set ") {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() && seen.insert(name.clone()) {
                items.push(CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some("mutable variable (set)".to_string()),
                    ..Default::default()
                });
            }
        }
    }

    for item in &module.items {
        if let ItemKind::Fun(f) = &item.kind {
            for p in &f.params {
                if p.is_ref && p.is_mut && seen.insert(p.name.name.clone()) {
                    items.push(CompletionItem {
                        label: p.name.name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: Some(format!("ref ~{}", p.ty.as_ref().map(|t| t.to_string()).unwrap_or_default())),
                        ..Default::default()
                    });
                }
            }
        }
    }

    if in_class {
        let cls_name = find_enclosing_class_context(offset, doc_text);
        for item in &module.items {
            if let ItemKind::Class(c) = &item.kind {
                if (item.span.start.0 as usize) <= offset && offset <= (item.span.end.0 as usize)
                    || cls_name.as_deref() == Some(&c.name.name)
                {
                    populate_class_rewrite_fields(items, c, &mut seen);
                }
            }
        }
        if let Some(ref name) = cls_name {
            populate_class_rewrite_fields_from_text(items, name, doc_text, &mut seen);
        }
        if seen.insert("this".to_string()) {
            items.push(CompletionItem {
                label: "this".to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("current instance".to_string()),
                ..Default::default()
            });
        }
    }
}

fn populate_class_rewrite_fields(
    items: &mut Vec<CompletionItem>,
    cls: &yukic_ast::ClassDecl,
    seen: &mut std::collections::HashSet<String>,
) {
    for f in &cls.fields {
        if f.is_static {
            continue;
        }
        let this_field = format!("this.{}", f.name.name);
        if seen.insert(this_field.clone()) {
            items.push(CompletionItem {
                label: this_field,
                kind: Some(CompletionItemKind::FIELD),
                detail: Some(format!("field {}: {}", f.name.name, f.ty)),
                ..Default::default()
            });
        }
    }
    for m in &cls.methods {
        for p in &m.params {
            if p.is_ref && p.is_mut && seen.insert(p.name.name.clone()) {
                items.push(CompletionItem {
                    label: p.name.name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("ref ~{}", p.ty.as_ref().map(|t| t.to_string()).unwrap_or_default())),
                    ..Default::default()
                });
            }
        }
    }
}

fn populate_in_scope_variables(
    items: &mut Vec<CompletionItem>,
    offset: usize,
    doc_text: &str,
    module: &Module,
    current_fun: Option<&str>,
    current_class: Option<&str>,
) {
    let mut seen = std::collections::HashSet::new();
    let safe_offset = offset.min(doc_text.len());
    let text_before = &doc_text[..safe_offset];

    // local vars decl before cursor
    for line in text_before.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }

        let decl_rest = if let Some(r) = trimmed.strip_prefix("set mut ") {
            Some(r)
        } else if let Some(r) = trimmed.strip_prefix("set ") {
            Some(r)
        } else if let Some(r) = trimmed.strip_prefix("static mut ") {
            Some(r)
        } else if let Some(r) = trimmed.strip_prefix("static ") {
            Some(r)
        } else {
            None
        };

        if let Some(rest) = decl_rest {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && seen.insert(name.clone()) {
                let ty_detail = infer_receiver_type(&name, doc_text)
                    .map(|ty| format!("variable {name}: {ty}"))
                    .unwrap_or_else(|| format!("variable {name}"));
                items.push(CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(ty_detail),
                    ..Default::default()
                });
            }
        }
    }

    // params of curr func or method
    if let Some(fn_name) = current_fun {
        for item in &module.items {
            match &item.kind {
                ItemKind::Fun(f) if f.name.name == fn_name => {
                    for p in &f.params {
                        if seen.insert(p.name.name.clone()) {
                            let ty_str = p.ty.as_ref().map(|t| t.to_string()).unwrap_or_else(|| "any".to_string());
                            items.push(CompletionItem {
                                label: p.name.name.clone(),
                                kind: Some(CompletionItemKind::VARIABLE),
                                detail: Some(format!("param {}: {}", p.name.name, ty_str)),
                                ..Default::default()
                            });
                        }
                    }
                }
                ItemKind::Class(c) => {
                    for m in &c.methods {
                        if m.name.name == fn_name {
                            for p in &m.params {
                                if seen.insert(p.name.name.clone()) {
                                    let ty_str = p.ty.as_ref().map(|t| t.to_string()).unwrap_or_else(|| "any".to_string());
                                    items.push(CompletionItem {
                                        label: p.name.name.clone(),
                                        kind: Some(CompletionItemKind::VARIABLE),
                                        detail: Some(format!("param {}: {}", p.name.name, ty_str)),
                                        ..Default::default()
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if current_class.is_some() && seen.insert("this".to_string()) {
        items.push(CompletionItem {
            label: "this".to_string(),
            kind: Some(CompletionItemKind::VARIABLE),
            detail: Some("current instance".to_string()),
            ..Default::default()
        });
    }
}

fn is_after_class_name(prefix: &str) -> bool {
    let line = prefix.lines().last().unwrap_or(prefix);
    if line.contains('{') || line.contains(';') || line.contains("extends") {
        return false;
    }
    let trimmed = line.trim_start();
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    let class_idx = words.iter().position(|&w| w == "class");
    if let Some(ci) = class_idx {
        if words.len() == ci + 2 {
            if line.ends_with(|c: char| c.is_whitespace()) {
                return true;
            }
        } else if words.len() == ci + 3 {
            let partial = words[ci + 2];
            if "extends".starts_with(partial) && partial != "extends" {
                return true;
            }
        }
    }
    false
}

fn is_init_position(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }
    let last = *words.last().unwrap();
    if last == "init" && prefix.ends_with(' ') {
        return true;
    }
    if let Some(prev) = words.iter().rev().nth(1).copied() {
        if prev == "init" && !last.contains('(') && !last.contains('{') && !last.contains(';') {
            return true;
        }
    }
    false
}

fn is_extends_position(prefix: &str) -> bool {
    let line = prefix.lines().last().unwrap_or(prefix);
    if line.contains('{') || line.contains(';') {
        return false;
    }
    let trimmed = line.trim_start();
    let is_class_decl = trimmed.starts_with("class ")
        || trimmed.starts_with("public class ")
        || trimmed.starts_with("locked class ")
        || trimmed.starts_with("public locked class ");
    if !is_class_decl {
        return false;
    }
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if let Some(ext_idx) = words.iter().position(|&w| w == "extends") {
        if let Some(with_idx) = words.iter().position(|&w| w == "with") {
            if ext_idx >= with_idx {
                return false;
            }
            return words.len() <= with_idx;
        }
        if words.len() > ext_idx + 1 {
            return true;
        }
        if words.len() == ext_idx + 1 && line.ends_with(|c: char| c.is_whitespace()) {
            return true;
        }
    }
    false
}

fn is_type_annotation_position(prefix: &str) -> bool {
    let last_line = prefix.lines().last().unwrap_or(prefix).trim_start();
    if last_line.starts_with("load ")
        || last_line.starts_with("public load ")
        || last_line.starts_with("cload ")
        || last_line.starts_with("public cload ")
    {
        return false;
    }

    let trimmed = prefix.trim_start();
    if let Some(colon_idx) = trimmed.rfind(':') {
        let after_colon = &trimmed[colon_idx + 1..];
        if !after_colon.contains('=') && !after_colon.contains('{') && !after_colon.contains(';') {
            let is_double_colon = (colon_idx > 0 && trimmed.as_bytes()[colon_idx - 1] == b':')
                || (colon_idx + 1 < trimmed.len() && trimmed.as_bytes()[colon_idx + 1] == b':');
            if !is_double_colon {
                return true;
            }
        }
    }
    if let Some(arrow_idx) = trimmed.rfind("->") {
        let after_arrow = &trimmed[arrow_idx + 2..];
        if !after_arrow.contains('{') && !after_arrow.contains(';') {
            return true;
        }
    }
    if let Some(as_idx) = trimmed.rfind(" as ") {
        let after_as = &trimmed[as_idx + 4..];
        if !after_as.contains(';') && !after_as.contains('{') {
            return true;
        }
    }

    false
}

fn extract_current_decl_name(prefix: &str) -> Option<String> {
    let line = prefix.lines().last().unwrap_or(prefix);
    let words: Vec<&str> = line.split_whitespace().collect();
    if let Some(pos) = words.iter().position(|&w| w == "class") {
        if let Some(&name) = words.get(pos + 1) {
            if name != "extends" && name != "{" && name != ":" {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn populate_non_locked_classes(
    items: &mut Vec<CompletionItem>,
    module: &Module,
    current_file: &Path,
    env: &WorkspaceEnv,
    current_decl_name: Option<&str>,
) {
    let mut seen = std::collections::HashSet::new();
    if let Some(curr) = current_decl_name {
        seen.insert(curr.to_string());
    }

    for item in &module.items {
        if let ItemKind::Class(ref c) = item.kind {
            if !c.is_locked && seen.insert(c.name.name.clone()) {
                items.push(CompletionItem {
                    label: c.name.name.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(format!("class {}", c.name.name)),
                    insert_text: Some(c.name.name.clone()),
                    insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
                    ..Default::default()
                });
            }
        }
    }

    populate_external_non_locked_classes(items, module, current_file, env, &mut seen);
}

fn populate_external_non_locked_classes(
    items: &mut Vec<CompletionItem>,
    module: &Module,
    current_file: &Path,
    env: &WorkspaceEnv,
    seen: &mut std::collections::HashSet<String>,
) {
    if let Some(parent) = current_file.parent() {
        for cls in crate::workspace::scan_spes_classes(parent) {
            if seen.insert(cls.clone()) {
                items.push(CompletionItem {
                    label: cls.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(format!("class {cls} (spes)")),
                    insert_text: Some(cls),
                    insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
                    ..Default::default()
                });
            }
        }
        scan_parent_dir_non_locked_classes(parent, current_file, items, seen);
    }

    for item in &module.items {
        if let ItemKind::Import(ref imp) = item.kind {
            if let Some(target_file) = env.resolve_import(&imp.path, current_file) {
                if let Some(cached) = env.get_or_parse_module(&target_file) {
                    for exp in &cached.exports {
                        if exp.kind == "class" && !exp.signature.starts_with("locked class") && seen.insert(exp.name.clone()) {
                            items.push(CompletionItem {
                                label: exp.name.clone(),
                                kind: Some(CompletionItemKind::CLASS),
                                detail: Some(exp.signature.clone()),
                                insert_text: Some(exp.name.clone()),
                                insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }
}

fn scan_parent_dir_non_locked_classes(
    parent: &Path,
    current_file: &Path,
    items: &mut Vec<CompletionItem>,
    seen: &mut std::collections::HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(parent) else { return; };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file() && p.extension().is_some_and(|e| e == "ykn") && p != current_file {
            if let Ok(text) = std::fs::read_to_string(&p) {
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("locked class ") || trimmed.starts_with("public locked class ") {
                        continue;
                    }
                    for kw in &["class ", "public class "] {
                        if let Some(rest) = trimmed.strip_prefix(kw) {
                            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                            if !name.is_empty() && seen.insert(name.clone()) {
                                items.push(CompletionItem {
                                    label: name.clone(),
                                    kind: Some(CompletionItemKind::CLASS),
                                    detail: Some(format!("class {name}")),
                                    insert_text: Some(name),
                                    insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

fn populate_type_symbols(items: &mut Vec<CompletionItem>, module: &Module, current_file: &Path) {
    if let Some(parent) = current_file.parent() {
        for cls in crate::workspace::scan_spes_classes(parent) {
            items.push(CompletionItem {
                label: cls.clone(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some(format!("class {cls} (spes)")),
                ..Default::default()
            });
        }
    }
    for item in &module.items {
        match &item.kind {
            ItemKind::Class(ref c) => {
                items.push(CompletionItem {
                    label: c.name.name.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(format!("class {}", c.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::Struct(ref s) => {
                items.push(CompletionItem {
                    label: s.name.name.clone(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("struct {}", s.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::Cstruct(ref cs) => {
                items.push(CompletionItem {
                    label: cs.name.name.clone(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("cstruct {}", cs.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::Enum(ref e) => {
                items.push(CompletionItem {
                    label: e.name.name.clone(),
                    kind: Some(CompletionItemKind::ENUM),
                    detail: Some(format!("enum {}", e.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::Interface(ref i) => {
                items.push(CompletionItem {
                    label: i.name.name.clone(),
                    kind: Some(CompletionItemKind::INTERFACE),
                    detail: Some(format!("interface {}", i.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::TypeAlias(ref t) => {
                items.push(CompletionItem {
                    label: t.name.name.clone(),
                    kind: Some(CompletionItemKind::TYPE_PARAMETER),
                    detail: Some(format!("type {}", t.name.name)),
                    ..Default::default()
                });
            }
            _ => {}
        }
    }
}

fn get_prefix_before_offset(text: &str, offset: usize) -> &str {
    let safe_offset = offset.min(text.len());
    let slice = &text[..safe_offset];
    let start = slice.rfind(|c: char| c == '\n' || c == ';').map(|idx| idx + 1).unwrap_or(0);
    slice[start..].trim_start()
}

fn is_inside_attribute(line_prefix: &str) -> bool {
    let last_attr_open = line_prefix
        .rfind("#[")
        .or_else(|| line_prefix.rfind("#!["))
        .or_else(|| line_prefix.rfind("!#["));

    if let Some(open_idx) = last_attr_open {
        let after = &line_prefix[open_idx..];
        !after.contains(']')
    } else {
        false
    }
}

fn is_inside_c_attribute(line_prefix: &str) -> bool {
    let pipe_count = line_prefix.chars().filter(|&c| c == '|').count();
    pipe_count % 2 == 1
}

#[allow(dead_code)]
enum LoadContext<'a> {
    PackageRoot(&'a str),
    Submodule { pkg: &'a str, partial: &'a str },
    Symbols { module_path: &'a str, partial: &'a str },
}

fn extract_load_context(line_prefix: &str) -> Option<LoadContext<'_>> {
    let trimmed = line_prefix.trim_start();
    let rest = if let Some(r) = trimmed.strip_prefix("public load ") {
        r
    } else if let Some(r) = trimmed.strip_prefix("load ") {
        r
    } else if trimmed == "load" || trimmed == "public load" {
        return Some(LoadContext::PackageRoot(""));
    } else {
        return None;
    };

    if let Some((before_brace, inside_brace)) = rest.split_once('{') {
        let inside = inside_brace.trim_end_matches('}').trim();
        let partial = inside.split(',').last().unwrap_or("").trim();
        if let Some(colon_idx) = before_brace.find(':') {
            let path_part = before_brace[..colon_idx].trim();
            return Some(LoadContext::Symbols {
                module_path: path_part,
                partial,
            });
        }
        let before_trimmed = before_brace.trim_end();
        let pkg = if before_trimmed.chars().all(|c| c == '.') {
            before_trimmed
        } else if before_trimmed.ends_with('.') {
            before_trimmed[..before_trimmed.len() - 1].trim_end()
        } else {
            before_trimmed
        };
        return Some(LoadContext::Submodule { pkg, partial });
    }

    if let Some(colon_idx) = rest.find(':') {
        let path_part = rest[..colon_idx].trim();
        let after_colon = rest[colon_idx + 1..].trim();
        let partial = after_colon.split(',').last().unwrap_or("").trim();
        return Some(LoadContext::Symbols {
            module_path: path_part,
            partial,
        });
    }

    if rest.starts_with('.') {
        let leading_dots = rest.chars().take_while(|&c| c == '.').count();
        if leading_dots == rest.len() {
            return Some(LoadContext::Submodule {
                pkg: rest,
                partial: "",
            });
        }
        let last_dot = rest.rfind('.').unwrap_or(0);
        if last_dot < leading_dots {
            return Some(LoadContext::Submodule {
                pkg: &rest[..leading_dots],
                partial: &rest[leading_dots..],
            });
        } else {
            return Some(LoadContext::Submodule {
                pkg: rest[..last_dot].trim(),
                partial: rest[last_dot + 1..].trim(),
            });
        }
    }

    if let Some(dot_idx) = rest.rfind('.') {
        let pkg_part = rest[..dot_idx].trim();
        let partial = rest[dot_idx + 1..].trim();
        return Some(LoadContext::Submodule {
            pkg: pkg_part,
            partial,
        });
    }

    Some(LoadContext::PackageRoot(rest.trim()))
}

fn resolve_submodule_completions(
    pkg: &str,
    current_file: &Path,
    env: &WorkspaceEnv,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let current_stem = current_file.file_stem().and_then(|s| s.to_str()).unwrap_or("");

    if pkg.starts_with('.') {
        let dot_count = pkg.chars().take_while(|&c| c == '.').count();
        if let Some(mut dir) = current_file.parent().map(|p| p.to_path_buf()) {
            let mut valid = true;
            for _ in 1..dot_count {
                if let Some(parent) = dir.parent() {
                    dir = parent.to_path_buf();
                } else {
                    valid = false;
                    break;
                }
            }
            if valid {
                let sub_path = &pkg[dot_count..];
                for seg in sub_path.split('.').filter(|s| !s.is_empty()) {
                    dir = dir.join(seg);
                }
                if dir.is_dir() {
                    populate_dir_module_completions(&mut items, &dir, pkg, current_file, current_stem);
                }
            }
        }
    } else if pkg == "root" || pkg.starts_with("root.") {
        let root_src = env.project_root.join("src");
        let base_dir = if root_src.is_dir() { root_src } else { env.project_root.clone() };
        let mut dir = base_dir;
        if pkg.starts_with("root.") {
            let sub_path = &pkg["root.".len()..];
            for seg in sub_path.split('.').filter(|s| !s.is_empty()) {
                dir = dir.join(seg);
            }
        }
        if dir.is_dir() {
            populate_dir_module_completions(&mut items, &dir, pkg, current_file, current_stem);
        }
    } else {
        let submodules = env.list_package_submodules(pkg);
        for submod in submodules {
            if submod != current_stem {
                items.push(CompletionItem {
                    label: submod.clone(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some(format!("{pkg}.{submod}")),
                    ..Default::default()
                });
            }
        }
    }

    dedup_completion_items(&mut items);
    items
}

fn populate_dir_module_completions(
    items: &mut Vec<CompletionItem>,
    dir: &Path,
    pkg: &str,
    current_file: &Path,
    current_stem: &str,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    for entry in entries.flatten() {
        let p = entry.path();
        if p == current_file {
            continue;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.is_empty() || stem.starts_with('.') || stem == "lib" || stem == "mod" {
            continue;
        }
        if p.is_file() && p.extension().is_some_and(|e| e == "ykn") {
            if dir == current_file.parent().unwrap_or(dir) && stem == current_stem {
                continue;
            }
            items.push(CompletionItem {
                label: stem.to_string(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some(format!("{pkg}.{stem}")),
                ..Default::default()
            });
        } else if p.is_dir() {
            items.push(CompletionItem {
                label: stem.to_string(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some(format!("{pkg}.{stem}")),
                ..Default::default()
            });
        }
    }
}

fn extract_scoped_path(line_prefix: &str) -> Option<(Vec<&str>, &str)> {
    let trimmed = line_prefix.trim_end();
    let dbl_colon = trimmed.rfind("::")?;

    let before_colon = &trimmed[..dbl_colon];
    let after_colon = &trimmed[dbl_colon + 2..];

    let start_idx = before_colon
        .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != ':')
        .map(|i| i + 1)
        .unwrap_or(0);
    let full_path = &before_colon[start_idx..];
    let segments: Vec<&str> = full_path.split("::").filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }

    Some((segments, after_colon.trim()))
}

struct CloadPathContext<'a> {
    path_start_byte: usize,
    partial: &'a str,
    has_closing_quote: bool,
}

fn extract_cload_path_context(doc_text: &str, offset: usize) -> Option<CloadPathContext<'_>> {
    let safe_offset = offset.min(doc_text.len());
    let line_start = doc_text[..safe_offset].rfind('\n').map_or(0, |i| i + 1);
    let line_prefix = &doc_text[line_start..safe_offset];
    let trimmed = line_prefix.trim_start();
    let leading_ws = line_prefix.len() - trimmed.len();

    let after_kw = if let Some(r) = trimmed.strip_prefix("public cload") {
        r
    } else if let Some(r) = trimmed.strip_prefix("cload") {
        r
    } else if let Some(r) = trimmed.strip_prefix("public load") {
        r
    } else if let Some(r) = trimmed.strip_prefix("load") {
        r
    } else {
        return None;
    };

    if after_kw.is_empty() || !after_kw.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }

    let kw_len = trimmed.len() - after_kw.len();
    let after_ws = after_kw.trim_start();
    let ws_len = after_kw.len() - after_ws.len();
    let after_open_quote = after_ws.strip_prefix('"')?;
    // If there's already another quote before the cursor, cursor is past the string literal.
    if after_open_quote.contains('"') {
        return None;
    }

    let path_start_byte = line_start + leading_ws + kw_len + ws_len + 1;
    let rest_of_line = doc_text[safe_offset..]
        .split('\n')
        .next()
        .unwrap_or("");
    let rest_stmt = rest_of_line.split(';').next().unwrap_or("");
    let has_closing_quote = rest_stmt.contains('"');

    Some(CloadPathContext {
        path_start_byte,
        partial: after_open_quote,
        has_closing_quote,
    })
}

fn populate_scoped_completions(
    items: &mut Vec<CompletionItem>,
    segments: &[&str],
    _partial: &str,
    module: &Module,
    env: &WorkspaceEnv,
    current_file: &Path,
) {
    if segments.is_empty() {
        return;
    }

    let first = segments[0];

    if segments.len() == 1 {
        for (_decl, hdr) in env.collect_cload_headers(module, current_file) {
            if hdr.namespace == first && hdr.error.is_none() {
                for export in hdr.exports {
                    push_export_completion(items, export);
                }
            }
        }
    }

    // if typing submodules under a package (e.g. `std::`)
    if segments.len() == 1 {
        let submodules = env.list_package_submodules(first);
        for submod in submodules {
            items.push(CompletionItem {
                label: submod.clone(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(format!("{first}::{submod}")),
                ..Default::default()
            });
        }
    }

    // Try resolving as a module import path: e.g. `std.io`
    let import_path_str = segments.join(".");
    let dummy_path = parse_import_path_str(&import_path_str);
    if let Some(target_file) = env.resolve_import(&dummy_path, current_file) {
        if let Some(cached) = env.get_or_parse_module(&target_file) {
            for export in cached.exports {
                push_export_completion(items, export);
            }
        }
    }
}

fn push_export_completion(items: &mut Vec<CompletionItem>, export: crate::workspace::ExportedSymbol) {
    let (insert_text, insert_format) = match export.kind.as_str() {
        "function" | "cfun" => {
            let (snip, fmt) = function_signature_to_snippet(&export.name, &export.signature);
            (Some(snip), fmt)
        }
        "struct" | "cstruct" | "cunion" => {
            let (snip, fmt) = struct_signature_to_snippet(&export.name, &export.signature);
            (Some(snip), fmt)
        }
        _ => (None, None),
    };
    items.push(CompletionItem {
        label: export.name,
        kind: match export.kind.as_str() {
            "function" | "cfun" => Some(CompletionItemKind::FUNCTION),
            "struct" | "cstruct" | "cunion" => Some(CompletionItemKind::STRUCT),
            "class" => Some(CompletionItemKind::CLASS),
            "enum" => Some(CompletionItemKind::ENUM),
            "interface" => Some(CompletionItemKind::INTERFACE),
            "constant" => Some(CompletionItemKind::CONSTANT),
            _ => Some(CompletionItemKind::VARIABLE),
        },
        detail: Some(export.signature),
        documentation: export.doc.map(tower_lsp::lsp_types::Documentation::String),
        insert_text,
        insert_text_format: insert_format,
        ..Default::default()
    });
}

fn extract_dot_receiver(line_prefix: &str) -> Option<(&str, &str)> {
    let trimmed = line_prefix.trim_end();
    let dot_idx = trimmed.rfind('.')?;

    if dot_idx > 0 && trimmed.as_bytes()[dot_idx - 1] == b'.' {
        return None;
    }
    if dot_idx + 1 < trimmed.len() && trimmed.as_bytes()[dot_idx + 1] == b'.' {
        return None;
    }
    if trimmed.starts_with("load ") || trimmed.starts_with("public load ") {
        return None;
    }

    let before_dot = &trimmed[..dot_idx];
    let after_dot = &trimmed[dot_idx + 1..];

    if after_dot.chars().any(|c| !c.is_alphanumeric() && c != '_') {
        return None;
    }

    let ident_start = before_dot
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| i + 1)
        .unwrap_or(0);
    let receiver = &before_dot[ident_start..];
    if receiver.is_empty() {
        return None;
    }

    Some((receiver, after_dot))
}

fn infer_receiver_type(receiver: &str, doc_text: &str) -> Option<String> {
    if receiver == "this" {
        return Some("this".to_string());
    }

    for line in doc_text.lines() {
        let trimmed = line.trim();
        // skip import lines and comments
        if trimmed.starts_with("load ")
            || trimmed.starts_with("public load ")
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
        {
            continue;
        }

        // check variable declarations: `set [mut] {receiver} ...` or `static [mut] {receiver} ...`
        let var_prefix = if let Some(rest) = trimmed.strip_prefix("set mut ") {
            Some(rest)
        } else if let Some(rest) = trimmed.strip_prefix("set ") {
            Some(rest)
        } else if let Some(rest) = trimmed.strip_prefix("static mut ") {
            Some(rest)
        } else if let Some(rest) = trimmed.strip_prefix("static ") {
            Some(rest)
        } else {
            None
        };

        if let Some(after_decl) = var_prefix {
            let with_colon = format!("{receiver}:");
            if let Some(rest) = after_decl.strip_prefix(&with_colon) {
                let ty = rest
                    .split(|c: char| c == '=' || c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .trim();
                if !ty.is_empty() {
                    return Some(ty.to_string());
                }
            }
            let with_colon_space = format!("{receiver} : ");
            if let Some(rest) = after_decl.strip_prefix(&with_colon_space) {
                let ty = rest
                    .split(|c: char| c == '=' || c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .trim();
                if !ty.is_empty() {
                    return Some(ty.to_string());
                }
            }

            let with_eq_init = format!("{receiver} = init ");
            if let Some(rest) = after_decl.strip_prefix(&with_eq_init) {
                let name = rest
                    .split(|c: char| c == '(' || c.is_whitespace() || c == '{')
                    .next()
                    .unwrap_or("")
                    .trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }

            let with_eq = format!("{receiver} = ");
            if let Some(rest) = after_decl.strip_prefix(&with_eq) {
                let raw_token = rest
                    .split(|c: char| c == '(' || c.is_whitespace() || c == '{')
                    .next()
                    .unwrap_or("")
                    .trim();
                let short_token = raw_token.rsplit("::").next().unwrap_or(raw_token);
                if !short_token.is_empty()
                    && short_token
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_uppercase())
                        .unwrap_or(false)
                {
                    return Some(short_token.to_string());
                }
            }
        }

        // check parameter declarations: `(..., {receiver}: Type, ...)`
        let param_pat = format!("{receiver}:");
        let mut search_from = 0;
        while let Some(rel_idx) = trimmed[search_from..].find(&param_pat) {
            let idx = search_from + rel_idx;
            let is_boundary = if idx == 0 {
                true
            } else {
                let prev_char = trimmed[..idx].chars().last().unwrap_or(' ');
                !prev_char.is_alphanumeric() && prev_char != '_'
            };

            if is_boundary {
                let rest = trimmed[idx + param_pat.len()..].trim_start();
                let ty = rest
                    .split(|c: char| c == ',' || c == ')' || c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .trim();
                if !ty.is_empty() && !ty.starts_with("ref") {
                    return Some(ty.to_string());
                }
            }
            search_from = idx + param_pat.len();
        }
    }

    None
}

fn populate_instance_dot_completions(
    items: &mut Vec<CompletionItem>,
    receiver: &str,
    module: &Module,
    current_file: &Path,
    env: &WorkspaceEnv,
    doc_text: &str,
) {
    let receiver_type = infer_receiver_type(receiver, doc_text).or_else(|| {
        if receiver == "this" {
            return Some("this".to_string());
        }
        for item in &module.items {
            if let ItemKind::Class(ref cls) = item.kind {
                if cls.name.name == receiver {
                    return Some(receiver.to_string());
                }
            }
            if let ItemKind::Struct(ref s) = item.kind {
                if s.name.name == receiver {
                    return Some(receiver.to_string());
                }
            }
        }
        for item in &module.items {
            if let ItemKind::Import(ref imp) = item.kind {
                if let Some(target_file) = env.resolve_import(&imp.path, current_file) {
                    if let Some(cached) = env.get_or_parse_module(&target_file) {
                        for export in &cached.exports {
                            if export.name == receiver {
                                return Some(receiver.to_string());
                            }
                        }
                    }
                }
            }
        }
        None
    });

    if let Some(ref ty_name) = receiver_type {
        if ty_name == "this" {
            for item in &module.items {
                if let ItemKind::Class(ref cls) = item.kind {
                    add_class_members(items, cls);
                }
                if let ItemKind::Struct(ref s) = item.kind {
                    add_struct_members(items, s);
                }
            }
            extract_class_members_from_text(doc_text, items);
            dedup_completion_items(items);
            return;
        }

        for item in &module.items {
            if let ItemKind::Class(ref cls) = item.kind {
                if cls.name.name == *ty_name {
                    add_class_members(items, cls);
                    return;
                }
            }
            if let ItemKind::Struct(ref s) = item.kind {
                if s.name.name == *ty_name {
                    add_struct_members(items, s);
                    return;
                }
            }
            if let ItemKind::Cstruct(ref cs) = item.kind {
                if cs.name.name == *ty_name {
                    add_cstruct_members(items, cs);
                    return;
                }
            }
            if let ItemKind::Externc(ref ext) = item.kind {
                for ei in &ext.items {
                    if let yukic_ast::ExterncItem::Cstruct(ref cs) = ei {
                        if cs.name.name == *ty_name {
                            add_cstruct_members(items, cs);
                            return;
                        }
                    }
                }
            }
        }

        for (_decl, hdr) in env.collect_cload_headers(module, current_file) {
            for hdr_item in &hdr.items {
                if let ItemKind::Externc(ref ext) = hdr_item.kind {
                    for ei in &ext.items {
                        if let yukic_ast::ExterncItem::Cstruct(ref cs) = ei {
                            if cs.name.name == *ty_name {
                                add_cstruct_members(items, cs);
                                return;
                            }
                        }
                    }
                }
            }
        }

        for item in &module.items {
            if let ItemKind::Import(ref imp) = item.kind {
                if let Some(target_file) = env.resolve_import(&imp.path, current_file) {
                    if let Some(cached) = env.get_or_parse_module(&target_file) {
                        for export in &cached.exports {
                            if export.name == *ty_name {
                                if let Some(target_mod) = env.get_or_parse_module(&export.file_path) {
                                    for target_item in &target_mod.module.items {
                                        if let ItemKind::Class(ref cls) = target_item.kind {
                                            if cls.name.name == *ty_name {
                                                add_class_members(items, cls);
                                                return;
                                            }
                                        }
                                        if let ItemKind::Struct(ref s) = target_item.kind {
                                            if s.name.name == *ty_name {
                                                add_struct_members(items, s);
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    add_common_object_methods(items);
    populate_dot_completions(items, module);
}

fn add_class_members(items: &mut Vec<CompletionItem>, cls: &yukic_ast::ClassDecl) {
    for m in &cls.methods {
        items.push(CompletionItem {
            label: m.name.name.clone(),
            kind: Some(CompletionItemKind::METHOD),
            detail: Some(format!("fun {}(...)", m.name.name)),
            insert_text: Some(function_snippet(&m.name.name, &m.params)),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        });
    }
    for a in &cls.accessors {
        items.push(CompletionItem {
            label: a.name.name.clone(),
            kind: Some(CompletionItemKind::PROPERTY),
            detail: Some(format!("property {}", a.name.name)),
            ..Default::default()
        });
    }
    for f in &cls.fields {
        items.push(CompletionItem {
            label: f.name.name.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(format!("field {}: {}", f.name.name, f.ty)),
            ..Default::default()
        });
    }
}

fn add_struct_members(items: &mut Vec<CompletionItem>, s: &yukic_ast::StructDecl) {
    for f in &s.fields {
        items.push(CompletionItem {
            label: f.name.name.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(format!("field {}: {}", f.name.name, f.ty)),
            ..Default::default()
        });
    }
    for m in &s.methods {
        items.push(CompletionItem {
            label: m.name.name.clone(),
            kind: Some(CompletionItemKind::METHOD),
            detail: Some(format!("fun {}(...)", m.name.name)),
            ..Default::default()
        });
    }
}

fn add_cstruct_members(items: &mut Vec<CompletionItem>, cs: &yukic_ast::CstructDecl) {
    for f in &cs.fields {
        items.push(CompletionItem {
            label: f.name.name.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(format!("field {}: {}", f.name.name, f.ty)),
            ..Default::default()
        });
    }
}

fn add_common_object_methods(items: &mut Vec<CompletionItem>) {
    items.push(CompletionItem {
        label: "len()".to_string(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some("Length of collection or string".to_string()),
        ..Default::default()
    });
    items.push(CompletionItem {
        label: "as_ptr()".to_string(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some("Get pointer to internal buffer".to_string()),
        ..Default::default()
    });
    items.push(CompletionItem {
        label: "cp()".to_string(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some("Clone / copy buffer in current region".to_string()),
        ..Default::default()
    });
}

fn populate_import_symbols(
    items: &mut Vec<CompletionItem>,
    import_path_str: &str,
    current_file: &Path,
    env: &WorkspaceEnv,
) {
    items.push(CompletionItem {
        label: "*".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("Import all public symbols".to_string()),
        ..Default::default()
    });

    let dummy_path = parse_import_path_str(import_path_str);
    if let Some(target_file) = env.resolve_import(&dummy_path, current_file) {
        if let Some(cached) = env.get_or_parse_module(&target_file) {
            for export in cached.exports {
                items.push(CompletionItem {
                    label: export.name,
                    kind: match export.kind.as_str() {
                        "function" | "cfun" => Some(CompletionItemKind::FUNCTION),
                        "struct" => Some(CompletionItemKind::STRUCT),
                        "class" => Some(CompletionItemKind::CLASS),
                        "enum" => Some(CompletionItemKind::ENUM),
                        "interface" => Some(CompletionItemKind::INTERFACE),
                        _ => Some(CompletionItemKind::VARIABLE),
                    },
                    detail: Some(export.signature),
                    documentation: export.doc.map(tower_lsp::lsp_types::Documentation::String),
                    ..Default::default()
                });
            }
        }
    }
}

fn populate_cload_symbols(
    items: &mut Vec<CompletionItem>,
    module: &Module,
    current_file: &Path,
    env: &WorkspaceEnv,
) {
    for (_decl, hdr) in env.collect_cload_headers(module, current_file) {
        if hdr.error.is_some() {
            continue;
        }
        items.push(CompletionItem {
            label: hdr.namespace.clone(),
            kind: Some(CompletionItemKind::MODULE),
            detail: Some(format!("C header namespace ({})", hdr.rel_path)),
            ..Default::default()
        });
        for export in hdr.exports {
            push_export_completion(items, export);
        }
    }
}

fn populate_imported_symbols(
    items: &mut Vec<CompletionItem>,
    module: &Module,
    current_file: &Path,
    env: &WorkspaceEnv,
    prefix: &str,
    current_fun: Option<&str>,
    current_class: Option<&str>,
) {
    for item in &module.items {
        if let ItemKind::Import(ref imp) = item.kind {
            if let Some(target_file) = env.resolve_import(&imp.path, current_file) {
                if let Some(cached) = env.get_or_parse_module(&target_file) {
                    for export in cached.exports {
                        let (matched, alias_opt) = match &imp.symbols {
                            None => (true, None),
                            Some(clause) => {
                                let mut m = false;
                                let mut a = None;
                                for (idx, it) in clause.items.iter().enumerate() {
                                    if it.pattern.matches(&export.name) {
                                        m = true;
                                        if let Some(alias) = clause.aliases.get(idx) {
                                            a = Some(alias.name.clone());
                                        }
                                        break;
                                    }
                                }
                                (m, a)
                            }
                        };
                        if matched {
                            let display_name = alias_opt.unwrap_or_else(|| export.name.clone());
                            if current_fun == Some(&display_name) || current_class == Some(&display_name) {
                                continue;
                            }
                            let (insert_text, insert_format) = match export.kind.as_str() {
                                "function" | "cfun" => {
                                    let (snip, fmt) = function_signature_to_snippet(&display_name, &export.signature);
                                    (Some(snip), fmt)
                                }
                                "struct" => {
                                    let (snip, fmt) = struct_signature_to_snippet(&display_name, &export.signature);
                                    (Some(snip), fmt)
                                }
                                "class" => {
                                    let init_params: Vec<yukic_ast::Param> = cached.module.items.iter()
                                        .filter_map(|it| match it.kind {
                                            ItemKind::Class(ref c) if c.name.name == export.name => {
                                                c.methods.iter().find(|m| m.name.name == "init").map(|m| m.params.clone())
                                            }
                                            _ => None,
                                        })
                                        .next()
                                        .unwrap_or_default();
                                    let (snip, fmt) = class_snippet_with_params(&display_name, &init_params, prefix);
                                    (Some(snip), fmt)
                                }
                                _ => (None, None),
                            };
                            items.push(CompletionItem {
                                label: display_name,
                                kind: match export.kind.as_str() {
                                    "function" | "cfun" => Some(CompletionItemKind::FUNCTION),
                                    "struct" => Some(CompletionItemKind::STRUCT),
                                    "class" => Some(CompletionItemKind::CLASS),
                                    "enum" => Some(CompletionItemKind::ENUM),
                                    "interface" => Some(CompletionItemKind::INTERFACE),
                                    _ => Some(CompletionItemKind::VARIABLE),
                                },
                                detail: Some(export.signature),
                                documentation: export.doc.map(tower_lsp::lsp_types::Documentation::String),
                                insert_text,
                                insert_text_format: insert_format,
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }
}

fn parse_import_path_str(s: &str) -> ImportPath {
    let dummy_span = yukic_span::Span::DUMMY;

    if s.starts_with('.') {
        // count leading dots
        let dots = s.chars().take_while(|&c| c == '.').count();
        let rel_level = dots.saturating_sub(1);
        let rest = &s[dots..];
        let segments = rest
            .split('.')
            .filter(|p| !p.is_empty())
            .map(|seg| yukic_ast::ImportPathSegment {
                pattern: ImportPattern::Exact(yukic_ast::Ident::new(seg, dummy_span)),
                force_folder: false,
                span: dummy_span,
            })
            .collect();

        ImportPath {
            prefix: PathPrefix::Relative(rel_level),
            segments,
            span: dummy_span,
        }
    } else {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() == 1 {
            ImportPath {
                prefix: PathPrefix::Package(yukic_ast::Ident::new(parts[0], dummy_span)),
                segments: Vec::new(),
                span: dummy_span,
            }
        } else {
            let prefix = PathPrefix::Package(yukic_ast::Ident::new(parts[0], dummy_span));
            let segments = parts[1..]
                .iter()
                .map(|seg| yukic_ast::ImportPathSegment {
                    pattern: ImportPattern::Exact(yukic_ast::Ident::new(*seg, dummy_span)),
                    force_folder: false,
                    span: dummy_span,
                })
                .collect();
            ImportPath {
                prefix,
                segments,
                span: dummy_span,
            }
        }
    }
}

fn populate_compiler_attributes(items: &mut Vec<CompletionItem>, prefix: &str) {
    if prefix.contains("cfg(") || prefix.contains("cfg_attr(") {
        let cfg_predicates = [
            ("all", "all(${1:cond1}, ${2:cond2})", "Logical AND for cfg conditions"),
            ("any", "any(${1:cond1}, ${2:cond2})", "Logical OR for cfg conditions"),
            ("not", "not(${1:cond})", "Logical NOT for cfg conditions"),
            ("target_os = \"linux\"", "target_os = \"linux\"", "Target OS Linux predicate"),
            ("target_os = \"windows\"", "target_os = \"windows\"", "Target OS Windows predicate"),
            ("target_os = \"macos\"", "target_os = \"macos\"", "Target OS macOS predicate"),
            ("target_arch = \"x86_64\"", "target_arch = \"x86_64\"", "Target architecture x86_64"),
            ("target_arch = \"aarch64\"", "target_arch = \"aarch64\"", "Target architecture AArch64"),
            ("target_env = \"gnu\"", "target_env = \"gnu\"", "GNU libc environment"),
            ("target_env = \"musl\"", "target_env = \"musl\"", "musl libc environment"),
            ("feature = \"...\"", "feature = \"${1:feature_name}\"", "Project manifest feature flag"),
        ];

        for (label, snippet, doc) in cfg_predicates {
            items.push(CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::PROPERTY),
                detail: Some(doc.to_string()),
                insert_text: Some(snippet.to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }
    }

    let attrs = [
        ("inline", "inline", "Inline hint for LLVM optimizer (inlinehint)"),
        ("inline_always", "inline_always", "Force inlining at any optimization level (alwaysinline)"),
        ("inline_never", "inline_never", "Strictly forbid inlining (noinline)"),
        ("no_inline", "no_inline", "Strictly forbid inlining (noinline)"),
        ("cold", "cold", "Mark as cold / unlikely execution branch"),
        ("noreturn", "noreturn", "Function never returns control to caller"),
        ("stable", "stable", "Pure function without side effects (nounwind, willreturn, readnone)"),
        ("fast_math", "fast_math", "Enable aggressive IEEE-754 FastMath optimizations (FMA, vectorization)"),
        ("no_region", "no_region", "Eliminate regional memory prologue/epilogue for zero-overhead in-place mutations"),
        ("thunk", "thunk", "Generate C ABI function pointer wrapper for threads and callbacks"),
        ("mangle", "mangle", "Force Yukino name mangling on cfun"),
        ("!mangle", "!mangle", "Disable automatic name mangling on fun, keeping raw symbol name"),
        ("test", "test", "Mark top-level function as a unit test for 'yukino test'"),
        ("should_panic", "should_panic", "Expect unit test to throw a panic"),
        ("should_panic = \"...\"", "should_panic = \"${1:expected error}\"", "Expect unit test to panic containing substring"),
        ("ignore", "ignore", "Temporarily skip unit test during execution"),
        ("ignore = \"...\"", "ignore = \"${1:reason}\"", "Temporarily skip unit test with an explanation"),
        ("timeout_ms = ...", "timeout_ms = ${1:1000}", "Set maximum unit test execution timeout in milliseconds"),
        ("cfg(...)", "cfg(${1:target_os = \"linux\"})", "Unified conditional compilation attribute"),
        ("cfg_attr(...)", "cfg_attr(${1:target_os = \"linux\"}, ${2:inline_always})", "Conditionally attach attribute when condition holds"),
        ("target = \"...\"", "target = \"${1:x86_64-unknown-linux-gnu}\"", "Shorthand filter by target triple"),
        ("arch = \"...\"", "arch = \"${1:x86_64}\"", "Shorthand filter by CPU architecture (\"x86_64\", \"arm64\")"),
        ("feature = \"...\"", "feature = \"${1:feature_name}\"", "Shorthand filter by manifest feature flag"),
        ("!target = \"...\"", "!target = \"${1:x86_64-unknown-linux-gnu}\"", "Negated target triple filter"),
        ("!arch = \"...\"", "!arch = \"${1:x86_64}\"", "Negated CPU architecture filter"),
        ("!feature = \"...\"", "!feature = \"${1:feature_name}\"", "Negated feature flag filter"),
        ("deprecated", "deprecated", "Mark API as deprecated"),
        ("deprecated = \"...\"", "deprecated = \"${1:Use alternative instead}\"", "Mark API as deprecated with explanation"),
        ("region::capacity(...)", "region::capacity(${1:1048576})", "Configure initial memory region bump allocation capacity in bytes"),
        ("lint::ignore(...)", "lint::ignore(\"${1:warning_code}\")", "Suppress specified compiler warning"),
        ("lint::allow(...)", "lint::allow(\"${1:warning_code}\")", "Allow specified compiler warning in this scope"),
        ("lint::deny(...)", "lint::deny(\"${1:warning_code}\")", "Treat specified compiler warning as error in this scope"),
    ];

    for (label, snippet, doc) in attrs {
        items.push(CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::PROPERTY),
            detail: Some(doc.to_string()),
            insert_text: Some(snippet.to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        });
    }
}

fn populate_c_attributes(items: &mut Vec<CompletionItem>) {
    let c_attrs = [
        ("cdecl", "cdecl", "Standard C calling convention (cdecl)"),
        ("stdcall", "stdcall", "Windows x86 standard calling convention (stdcall)"),
        ("fastcall", "fastcall", "Register-passed calling convention (fastcall)"),
        ("thiscall", "thiscall", "C++ method calling convention (thiscall)"),
        ("sysv64", "sysv64", "System V AMD64 ABI calling convention"),
        ("win64", "win64", "Microsoft x64 calling convention"),
        ("packed", "packed", "Pack struct fields without alignment padding (align 1)"),
        ("align = \"...\"", "align = \"${1:16}\"", "Explicit structure memory alignment"),
        ("weak", "weak", "Weak symbol linkage (weak)"),
        ("returns_nonnull", "returns_nonnull", "Guarantee returned pointer is non-null"),
    ];

    for (label, snippet, doc) in c_attrs {
        items.push(CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::PROPERTY),
            detail: Some(doc.to_string()),
            insert_text: Some(snippet.to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        });
    }
}

fn populate_dot_completions(items: &mut Vec<CompletionItem>, module: &Module) {
    items.push(CompletionItem {
        label: "as_ptr()".to_string(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some("Get pointer to internal buffer".to_string()),
        ..Default::default()
    });
    items.push(CompletionItem {
        label: "len()".to_string(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some("Length of collection or string".to_string()),
        ..Default::default()
    });
    items.push(CompletionItem {
        label: "cp()".to_string(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some("Clone / copy collection buffer in current region".to_string()),
        ..Default::default()
    });

    for item in &module.items {
        if let ItemKind::Class(ref cls) = item.kind {
            for method in &cls.methods {
                items.push(CompletionItem {
                    label: method.name.name.clone(),
                    kind: Some(CompletionItemKind::METHOD),
                    detail: Some(format!("Method in class {}", cls.name.name)),
                    ..Default::default()
                });
            }
            for accessor in &cls.accessors {
                items.push(CompletionItem {
                    label: accessor.name.name.clone(),
                    kind: Some(CompletionItemKind::PROPERTY),
                    detail: Some(format!("Property accessor in class {}", cls.name.name)),
                    ..Default::default()
                });
            }
            for field in &cls.fields {
                let kind_label = if field.is_static { "Static field" } else { "Field" };
                items.push(CompletionItem {
                    label: field.name.name.clone(),
                    kind: Some(if field.is_static {
                        CompletionItemKind::CONSTANT
                    } else {
                        CompletionItemKind::FIELD
                    }),
                    detail: Some(format!("{kind_label} {}: {} in class {}", field.name.name, field.ty, cls.name.name)),
                    ..Default::default()
                });
            }
        }
        if let ItemKind::Struct(ref s) = item.kind {
            for field in &s.fields {
                items.push(CompletionItem {
                    label: field.name.name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("Field in struct {}", s.name.name)),
                    ..Default::default()
                });
            }
            for method in &s.methods {
                items.push(CompletionItem {
                    label: method.name.name.clone(),
                    kind: Some(CompletionItemKind::METHOD),
                    detail: Some(format!("Method in struct {}", s.name.name)),
                    ..Default::default()
                });
            }
        }
        if let ItemKind::Cstruct(ref cs) = item.kind {
            for field in &cs.fields {
                items.push(CompletionItem {
                    label: field.name.name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("Field in cstruct {}", cs.name.name)),
                    ..Default::default()
                });
            }
        }
    }
}

fn function_snippet(name: &str, params: &[yukic_ast::Param]) -> String {
    if params.is_empty() {
        format!("{name}()$0")
    } else {
        let p_snippets: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(idx, p)| {
                let ty_str = p.ty.as_ref().map(|t| format!(": {t}")).unwrap_or_default();
                format!("${{{}:{}{ty_str}}}", idx + 1, p.name.name)
            })
            .collect();
        format!("{name}({})", p_snippets.join(", "))
    }
}

fn cfunction_snippet(name: &str, params: &[yukic_ast::CParam]) -> String {
    if params.is_empty() {
        format!("{name}()$0")
    } else {
        let p_snippets: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(idx, p)| format!("${{{}:{}: {}}}", idx + 1, p.name.name, p.ty))
            .collect();
        format!("{name}({})", p_snippets.join(", "))
    }
}

fn struct_snippet(name: &str, fields: &[yukic_ast::FieldDef]) -> String {
    if fields.is_empty() {
        format!("{name} {{\n    $0\n}}")
    } else {
        let f_snippets: Vec<String> = fields
            .iter()
            .enumerate()
            .map(|(idx, f)| format!("    {}: ${{{}:{}}},", f.name.name, idx + 1, f.ty))
            .collect();
        format!("{name} {{\n{}\n}}", f_snippets.join("\n"))
    }
}

fn class_snippet_with_params(
    name: &str,
    params: &[yukic_ast::Param],
    prefix: &str,
) -> (String, Option<InsertTextFormat>) {
    let trimmed = prefix.trim_end();
    let needs_init = trimmed.ends_with('=') || trimmed.ends_with("= ") || trimmed.split_whitespace().last() == Some("=");
    let prefix_str = if needs_init && !trimmed.ends_with("init") && !trimmed.ends_with("init ") {
        "init "
    } else {
        ""
    };

    if params.is_empty() {
        return (format!("{prefix_str}{name}()$0"), Some(InsertTextFormat::SNIPPET));
    }

    let mut parts = Vec::new();
    for (i, p) in params.iter().enumerate() {
        let idx = i + 1;
        let text = if let Some(ref ty) = p.ty {
            format!("{}: {}", p.name.name, ty)
        } else {
            p.name.name.clone()
        };
        parts.push(format!("${{{idx}:{text}}}"));
    }
    (
        format!("{prefix_str}{name}({})$0", parts.join(", ")),
        Some(InsertTextFormat::SNIPPET),
    )
}

fn class_snippet(name: &str, prefix: &str) -> (String, Option<InsertTextFormat>) {
    class_snippet_with_params(name, &[], prefix)
}

fn function_signature_to_snippet(name: &str, sig: &str) -> (String, Option<InsertTextFormat>) {
    if let Some(paren_idx) = sig.find('(') {
        if let Some(end_paren) = sig.rfind(')') {
            let params_str = sig[paren_idx + 1..end_paren].trim();
            if params_str.is_empty() {
                return (format!("{name}()$0"), Some(InsertTextFormat::SNIPPET));
            }
            let params: Vec<&str> = params_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            let mut snippets = Vec::new();
            for (idx, p) in params.iter().enumerate() {
                let clean_p = if let Some(eq) = p.find('=') {
                    p[..eq].trim()
                } else {
                    *p
                };
                snippets.push(format!("${{{}:{clean_p}}}", idx + 1));
            }
            return (
                format!("{name}({})", snippets.join(", ")),
                Some(InsertTextFormat::SNIPPET),
            );
        }
    }
    (format!("{name}()$0"), Some(InsertTextFormat::SNIPPET))
}

fn struct_signature_to_snippet(name: &str, sig: &str) -> (String, Option<InsertTextFormat>) {
    if let Some(brace_idx) = sig.find('{') {
        if let Some(end_brace) = sig.rfind('}') {
            let fields_str = sig[brace_idx + 1..end_brace].trim();
            let fields: Vec<&str> = fields_str
                .split(|c: char| c == ',' || c == '\n' || c == ';')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if !fields.is_empty() {
                let mut lines = Vec::new();
                for (idx, f) in fields.iter().enumerate() {
                    let clean = f.strip_prefix("public ").unwrap_or(f);
                    if let Some(colon) = clean.find(':') {
                        let fn_name = clean[..colon].trim();
                        let fn_ty = clean[colon + 1..].trim();
                        lines.push(format!("    {fn_name}: ${{{}:{fn_ty}}},", idx + 1));
                    }
                }
                if !lines.is_empty() {
                    return (
                        format!("{name} {{\n{}\n}}", lines.join("\n")),
                        Some(InsertTextFormat::SNIPPET),
                    );
                }
            }
        }
    }
    (format!("{name} {{\n    $0\n}}"), Some(InsertTextFormat::SNIPPET))
}

fn populate_available_classes(
    items: &mut Vec<CompletionItem>,
    module: &Module,
    current_file: &Path,
    env: &WorkspaceEnv,
    current_class: Option<&str>,
) {
    let mut seen = std::collections::HashSet::new();
    if let Some(cls) = current_class {
        seen.insert(cls.to_string());
    }

    for item in &module.items {
        if let ItemKind::Class(ref c) = item.kind {
            if seen.insert(c.name.name.clone()) {
                let init_params: Vec<yukic_ast::Param> = c.methods.iter()
                    .find(|m| m.name.name == "init")
                    .map(|m| m.params.clone())
                    .unwrap_or_default();
                let (snippet, fmt) = class_snippet_with_params(&c.name.name, &init_params, "init ");
                items.push(CompletionItem {
                    label: c.name.name.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(format!("class {}", c.name.name)),
                    insert_text: Some(snippet),
                    insert_text_format: fmt,
                    ..Default::default()
                });
            }
        }
    }

    if let Some(parent) = current_file.parent() {
        for cls in crate::workspace::scan_spes_classes(parent) {
            if seen.insert(cls.clone()) {
                items.push(CompletionItem {
                    label: cls.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(format!("class {cls} (spes)")),
                    insert_text: Some(format!("{cls}(${{1:}})")),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..Default::default()
                });
            }
        }
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() && p.extension().is_some_and(|e| e == "ykn") && p != current_file {
                    if let Ok(text) = std::fs::read_to_string(&p) {
                        for line in text.lines() {
                            let trimmed = line.trim();
                            for kw in &["class ", "public class "] {
                                if let Some(rest) = trimmed.strip_prefix(kw) {
                                    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                                    if !name.is_empty() && seen.insert(name.clone()) {
                                        items.push(CompletionItem {
                                            label: name.clone(),
                                            kind: Some(CompletionItemKind::CLASS),
                                            detail: Some(format!("class {name}")),
                                            insert_text: Some(format!("{name}(${{1:}})")),
                                            insert_text_format: Some(InsertTextFormat::SNIPPET),
                                            ..Default::default()
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    for item in &module.items {
        if let ItemKind::Import(ref imp) = item.kind {
            if let Some(target_file) = env.resolve_import(&imp.path, current_file) {
                if let Some(cached) = env.get_or_parse_module(&target_file) {
                    for export in &cached.exports {
                        if export.kind == "class" && seen.insert(export.name.clone()) {
                            let init_params: Vec<yukic_ast::Param> = cached.module.items.iter()
                                .filter_map(|it| match it.kind {
                                    ItemKind::Class(ref c) if c.name.name == export.name => {
                                        c.methods.iter().find(|m| m.name.name == "init").map(|m| m.params.clone())
                                    }
                                    _ => None,
                                })
                                .next()
                                .unwrap_or_default();
                            let (snippet, fmt) = class_snippet_with_params(&export.name, &init_params, "init ");
                            items.push(CompletionItem {
                                label: export.name.clone(),
                                kind: Some(CompletionItemKind::CLASS),
                                detail: Some(export.signature.clone()),
                                insert_text: Some(snippet),
                                insert_text_format: fmt,
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }
}

fn extract_class_members_from_text(doc_text: &str, items: &mut Vec<CompletionItem>) {
    for line in doc_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let field_line = trimmed.strip_prefix("public ").unwrap_or(trimmed);
        if let Some(colon_idx) = field_line.find(':') {
            let name_part = field_line[..colon_idx].trim();
            let ty_part = field_line[colon_idx + 1..].trim();
            if !name_part.contains(' ')
                && !name_part.is_empty()
                && !ty_part.is_empty()
                && !name_part.starts_with("fun")
                && !name_part.starts_with("load")
                && !name_part.starts_with("case")
                && !name_part.starts_with("set")
                && !name_part.starts_with("rewrite")
                && !name_part.starts_with("static")
            {
                items.push(CompletionItem {
                    label: name_part.to_string(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("field {name_part}: {ty_part}")),
                    ..Default::default()
                });
            }
        }
        let meth_line = trimmed.strip_prefix("public ").unwrap_or(trimmed);
        if let Some(rest) = meth_line.strip_prefix("fun ") {
            if let Some(paren_idx) = rest.find('(') {
                let name = rest[..paren_idx].trim();
                if !name.is_empty() && name != "init" {
                    items.push(CompletionItem {
                        label: name.to_string(),
                        kind: Some(CompletionItemKind::METHOD),
                        detail: Some(format!("fun {}(...)", name)),
                        insert_text: Some(format!("{name}($0)")),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        ..Default::default()
                    });
                }
            }
        }
    }
}

fn dedup_completion_items(items: &mut Vec<CompletionItem>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|it| seen.insert(it.label.clone()));
}

fn populate_ast_symbols(
    items: &mut Vec<CompletionItem>,
    module: &Module,
    prefix: &str,
    current_fun: Option<&str>,
    current_class: Option<&str>,
) {
    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) => {
                if current_fun == Some(f.name.name.as_str()) {
                    continue;
                }
                items.push(CompletionItem {
                    label: f.name.name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some(format!("fun {}", f.name.name)),
                    insert_text: Some(function_snippet(&f.name.name, &f.params)),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..Default::default()
                });
            }
            ItemKind::Cfun(ref c) => {
                if current_fun == Some(c.name.name.as_str()) {
                    continue;
                }
                items.push(CompletionItem {
                    label: c.name.name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some(format!("cfun {}", c.name.name)),
                    insert_text: Some(cfunction_snippet(&c.name.name, &c.params)),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..Default::default()
                });
            }
            ItemKind::Struct(ref s) => {
                items.push(CompletionItem {
                    label: s.name.name.clone(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("struct {}", s.name.name)),
                    insert_text: Some(struct_snippet(&s.name.name, &s.fields)),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..Default::default()
                });
            }
            ItemKind::Cstruct(ref cs) => {
                items.push(CompletionItem {
                    label: cs.name.name.clone(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("cstruct {}", cs.name.name)),
                    insert_text: Some(struct_snippet(&cs.name.name, &cs.fields)),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..Default::default()
                });
            }
            ItemKind::Class(ref c) => {
                if current_class == Some(c.name.name.as_str()) {
                    continue;
                }
                let init_params: Vec<yukic_ast::Param> = c.methods.iter()
                    .find(|m| m.name.name == "init")
                    .map(|m| m.params.clone())
                    .unwrap_or_default();
                let (snippet, fmt) = class_snippet_with_params(&c.name.name, &init_params, prefix);
                items.push(CompletionItem {
                    label: c.name.name.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(format!("class {}", c.name.name)),
                    insert_text: Some(snippet),
                    insert_text_format: fmt,
                    ..Default::default()
                });
            }
            ItemKind::Enum(ref e) => {
                items.push(CompletionItem {
                    label: e.name.name.clone(),
                    kind: Some(CompletionItemKind::ENUM),
                    detail: Some(format!("enum {}", e.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::Interface(ref i) => {
                items.push(CompletionItem {
                    label: i.name.name.clone(),
                    kind: Some(CompletionItemKind::INTERFACE),
                    detail: Some(format!("interface {}", i.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::Static(ref s) => {
                items.push(CompletionItem {
                    label: s.name.name.clone(),
                    kind: Some(CompletionItemKind::CONSTANT),
                    detail: Some(format!("static {}", s.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::Macro(ref m) => {
                items.push(CompletionItem {
                    label: m.name.name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some(format!("define {}!(...)", m.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::TypeAlias(ref t) => {
                items.push(CompletionItem {
                    label: t.name.name.clone(),
                    kind: Some(CompletionItemKind::TYPE_PARAMETER),
                    detail: Some(format!("type {}", t.name.name)),
                    ..Default::default()
                });
            }
            ItemKind::Alias(ref a) => {
                items.push(CompletionItem {
                    label: a.alias.name.clone(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some(format!("alias as {}", a.alias.name)),
                    ..Default::default()
                });
            }
            ItemKind::Externc(ref ext) => {
                for ext_item in &ext.items {
                    match ext_item {
                        yukic_ast::ExterncItem::Cfun(ref c) => {
                            items.push(CompletionItem {
                                label: c.name.name.clone(),
                                kind: Some(CompletionItemKind::FUNCTION),
                                detail: Some(format!("cfun {}", c.name.name)),
                                ..Default::default()
                            });
                        }
                        yukic_ast::ExterncItem::Cstruct(ref cs) => {
                            let kw = if cs.is_union { "cunion" } else { "cstruct" };
                            items.push(CompletionItem {
                                label: cs.name.name.clone(),
                                kind: Some(CompletionItemKind::STRUCT),
                                detail: Some(format!("{kw} {}", cs.name.name)),
                                ..Default::default()
                            });
                        }
                        yukic_ast::ExterncItem::Cvar(ref cv) => {
                            items.push(CompletionItem {
                                label: cv.name.name.clone(),
                                kind: Some(CompletionItemKind::VARIABLE),
                                detail: Some(format!("cvar {}: {}", cv.name.name, cv.ty)),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
            ItemKind::Cvar(ref cv) => {
                items.push(CompletionItem {
                    label: cv.name.name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("cvar {}: {}", cv.name.name, cv.ty)),
                    ..Default::default()
                });
            }
            _ => {}
        }
    }
}

fn populate_keywords(items: &mut Vec<CompletionItem>, prefix: &str) {
    let after_public = prefix.trim_end().ends_with("public");
    let class_init_snippet = if after_public {
        "class ${1:Name} {\n    public fun init($2) {\n        $0\n    }\n}"
    } else {
        "class ${1:Name} {\n    fun init($2) {\n        $0\n    }\n}"
    };
    let locked_class_init_snippet = if after_public {
        "locked class ${1:Name} {\n    public fun init($2) {\n        $0\n    }\n}"
    } else {
        "locked class ${1:Name} {\n    fun init($2) {\n        $0\n    }\n}"
    };

    let kw = [
        ("fun", "fun ${1:name}(${2}) {\n    $0\n}", "Declare a function"),
        ("public fun", "public fun ${1:name}(${2}) {\n    $0\n}", "Declare a public function"),
        ("cfun", "cfun ${1:name}(${2}) {\n    $0\n}", "Declare a C ABI function"),
        ("class", class_init_snippet, "Declare a class"),
        ("public class", "public class ${1:Name} {\n    public fun init($2) {\n        $0\n    }\n}", "Declare a public class"),
        ("locked class", locked_class_init_snippet, "Declare a non-inheritable locked class"),
        ("public locked class", "public locked class ${1:Name} {\n    public fun init($2) {\n        $0\n    }\n}", "Declare a public non-inheritable locked class"),
        ("struct", "struct ${1:Name} {\n    $0\n}", "Declare a value type structure"),
        ("public struct", "public struct ${1:Name} {\n    $0\n}", "Declare a public value type structure"),
        ("cstruct", "cstruct ${1:Name} {\n    $0\n}", "Declare a C-compatible structure"),
        ("cunion", "cunion ${1:Name} {\n    $0\n}", "Declare a C-compatible union"),
        ("cvar", "cvar ${1:name}: ${2:c_int};", "Declare an external C global variable"),
        ("interface", "interface ${1:Name} {\n    $0\n}", "Declare an interface contract"),
        ("enum", "enum ${1:Name} {\n    $0\n}", "Declare an enumeration"),
        ("load", "load ${1:module}: ${2:symbol};", "Import module symbols"),
        ("public load", "public load ${1:module}: *;", "Re-export module symbols"),
        ("set", "set ${1:var} = ${2:val};", "Declare an immutable variable"),
        ("static", "static ${1:NAME}: ${2:type} = ${3:val};", "Declare a global constant"),
        ("if", "if ${1:cond} {\n    $0\n}", "Conditional branch"),
        ("elif", "elif ${1:cond} {\n    $0\n}", "Alternative conditional branch"),
        ("else", "else {\n    $0\n}", "Fallback branch"),
        ("while", "while ${1:cond} {\n    $0\n}", "While loop"),
        ("for", "for ${1:item} in ${2:iter} {\n    $0\n}", "For-in iterator loop"),
        ("loop", "loop {\n    $0\n}", "Infinite loop"),
        ("switch", "switch ${1:val} {\n    ${2:pattern} -> ${3:expr}\n    else -> ${4:fallback}\n}", "Pattern matching"),
        ("sus", "sus {\n    $0\n} catch e: ${1:Error} {\n    eprintln(e.message);\n}", "Guarded exception block"),
        ("return", "return ${1:val};", "Return from function"),
        ("defer", "defer {\n    $0\n}", "Deferred cleanup execution"),
        ("region", "region {\n    $0\n}", "Scoped memory region block"),
        ("space", "space;", "Empty statement / placeholder"),
    ];

    for (label, snippet, doc) in kw {
        items.push(CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(doc.to_string()),
            insert_text: Some(snippet.to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        });
    }

    items.push(CompletionItem {
        label: "cload".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("Load C header (.h)".to_string()),
        insert_text: Some("cload \"$0\"".to_string()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        command: Some(Command {
            title: "triggerSuggest".to_string(),
            command: "editor.action.triggerSuggest".to_string(),
            arguments: None,
        }),
        ..Default::default()
    });
    items.push(CompletionItem {
        label: "public cload".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("Load and re-export C header (.h)".to_string()),
        insert_text: Some("public cload \"$0\"".to_string()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        command: Some(Command {
            title: "triggerSuggest".to_string(),
            command: "editor.action.triggerSuggest".to_string(),
            arguments: None,
        }),
        ..Default::default()
    });

    items.push(CompletionItem {
        label: "rewrite".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("Mutate an existing variable or field".to_string()),
        insert_text: Some("rewrite ".to_string()),
        insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
        command: Some(Command {
            title: "triggerSuggest".to_string(),
            command: "editor.action.triggerSuggest".to_string(),
            arguments: None,
        }),
        ..Default::default()
    });
}

fn populate_types(items: &mut Vec<CompletionItem>) {
    let types = [
        ("int8", "Signed 8-bit integer"),
        ("int16", "Signed 16-bit integer"),
        ("int32", "Signed 32-bit integer"),
        ("int64", "Signed 64-bit integer"),
        ("uint8", "Unsigned 8-bit integer"),
        ("uint16", "Unsigned 16-bit integer"),
        ("uint32", "Unsigned 32-bit integer"),
        ("uint64", "Unsigned 64-bit integer"),
        ("float32", "Single-precision 32-bit float"),
        ("float64", "Double-precision 64-bit float"),
        ("bool", "Boolean (true / false)"),
        ("string", "UTF-8 string"),
        ("bytes", "Raw byte buffer"),
        ("c_char", "C platform char"),
        ("c_schar", "C signed char"),
        ("c_uchar", "C unsigned char"),
        ("c_short", "C signed short"),
        ("c_ushort", "C unsigned short"),
        ("c_int", "C signed int"),
        ("c_uint", "C unsigned int"),
        ("c_long", "C signed long"),
        ("c_ulong", "C unsigned long"),
        ("c_longlong", "C signed long long"),
        ("c_ulonglong", "C unsigned long long"),
        ("c_size_t", "C size_t"),
        ("c_ssize_t", "C ssize_t"),
        ("c_bool", "C _Bool"),
        ("c_void", "C void"),
        ("ptr<...>", "ptr<${1:uint8}>"),
    ];

    for (t, doc) in types {
        items.push(CompletionItem {
            label: t.to_string(),
            kind: Some(CompletionItemKind::CLASS),
            detail: Some(doc.to_string()),
            insert_text: Some(t.to_string()),
            ..Default::default()
        });
    }
}

fn populate_intrinsics(items: &mut Vec<CompletionItem>) {
    let intrinsics = [
        ("file!", "file!()", "Current source file path macro"),
        ("line!", "line!()", "Current line number macro"),
        ("cfg!", "cfg!(${1:os = \"linux\"})", "Compile-time configuration check macro"),
        ("compile_print!", "compile_print!(\"${1:message}\")", "Compile-time stdout message macro"),
        ("include_str!", "include_str!(\"${1:path}\")", "Embed UTF-8 text file as string macro"),
        ("include_bytes!", "include_bytes!(\"${1:path}\")", "Embed binary file as bytes macro"),
    ];

    for (name, snippet, doc) in intrinsics {
        items.push(CompletionItem {
            label: name.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(doc.to_string()),
            insert_text: Some(snippet.to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        });
    }
}

fn populate_attribute_snippets(items: &mut Vec<CompletionItem>) {
    items.push(CompletionItem {
        label: "#[test]".to_string(),
        kind: Some(CompletionItemKind::SNIPPET),
        detail: Some("Unit test annotation".to_string()),
        insert_text: Some("#[test]\nfun test_${1:name}() {\n    $0\n}".to_string()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_load_context() {
        assert!(matches!(
            extract_load_context("load "),
            Some(LoadContext::PackageRoot(""))
        ));
        assert!(matches!(
            extract_load_context("load s"),
            Some(LoadContext::PackageRoot("s"))
        ));
        assert!(matches!(
            extract_load_context("load std."),
            Some(LoadContext::Submodule { pkg: "std", partial: "" })
        ));
        assert!(matches!(
            extract_load_context("load std.io: "),
            Some(LoadContext::Symbols { module_path: "std.io", partial: "" })
        ));
    }

    #[test]
    fn test_extract_scoped_path() {
        let (segs, partial) = extract_scoped_path("std::io::").unwrap();
        assert_eq!(segs, vec!["std", "io"]);
        assert_eq!(partial, "");

        let (segs2, partial2) = extract_scoped_path("std::io::pri").unwrap();
        assert_eq!(segs2, vec!["std", "io"]);
        assert_eq!(partial2, "pri");
    }

    #[test]
    fn test_is_type_annotation_position() {
        assert!(is_type_annotation_position("set x: "));
        assert!(is_type_annotation_position("set x: i"));
        assert!(is_type_annotation_position("fun foo(a: "));
        assert!(is_type_annotation_position("fun foo() -> "));
        assert!(!is_type_annotation_position("load std.io: "));
        assert!(!is_type_annotation_position("std::io::print"));
    }

    #[test]
    fn test_extract_dot_receiver() {
        assert_eq!(extract_dot_receiver("app."), Some(("app", "")));
        assert_eq!(extract_dot_receiver("app.re"), Some(("app", "re")));
        assert_eq!(extract_dot_receiver("this.width"), Some(("this", "width")));
        assert_eq!(extract_dot_receiver("0..10"), None);
    }

    #[test]
    fn test_load_std_completion() {
        let env = WorkspaceEnv::for_path(Path::new("/home/shareui/dev/yuki-lang/source/offlibs/siesta/apps/demosecond/src/main.ykn"));
        let submodules = env.list_package_submodules("std");
        println!("submodules of std: {:?}", submodules);
        assert!(!submodules.is_empty(), "std submodules should not be empty!");

        let text = "load std.";
        let doc = Document::new(tower_lsp::lsp_types::Url::parse("file:///home/shareui/dev/yuki-lang/source/offlibs/siesta/apps/demosecond/src/main.ykn").unwrap(), text.to_string(), 1);
        let (module, _) = crate::diagnostics::analyze_document(&doc, Some(&env));
        let res = compute_completions(&doc, Position::new(0, 9), &module, &env);
        match res {
            CompletionResponse::Array(items) => {
                println!("completion items for 'load std.': {:?}", items.iter().map(|i| &i.label).collect::<Vec<_>>());
                assert!(!items.is_empty());
            }
            _ => panic!("not array"),
        }
    }

    #[test]
    fn test_class_declaration_snippets() {
        let mut items = Vec::new();
        populate_keywords(&mut items, "");
        let cls = items.iter().find(|i| i.label == "class").unwrap();
        assert!(cls.insert_text.as_ref().unwrap().contains("fun init("));

        let lcls = items.iter().find(|i| i.label == "locked class").unwrap();
        assert!(lcls.insert_text.as_ref().unwrap().contains("fun init("));

        let pcls = items.iter().find(|i| i.label == "public class").unwrap();
        assert!(pcls.insert_text.as_ref().unwrap().contains("public fun init("));

        let mut items_after_pub = Vec::new();
        populate_keywords(&mut items_after_pub, "public ");
        let cls_pub = items_after_pub.iter().find(|i| i.label == "class").unwrap();
        assert!(cls_pub.insert_text.as_ref().unwrap().contains("public fun init("));
    }

    #[test]
    fn test_exm_test_file_completion() {
        let path = Path::new("/home/shareui/dev/yuki-lang/source/exm/src/utils/test.ykn");
        let uri = tower_lsp::lsp_types::Url::from_file_path(path).unwrap();
        let content = "load std.math: abs_int\n\npublic class Test {\n    num: int64\n    \n    public fun init(a: int64) {\n        rewrite this.num = a;\n    }\n\n    public fun eabsbsbsb() {\n        rewrite this.num = a\n    }\n}\n";
        let doc = Document::new(uri, content.to_string(), 1);
        let env = WorkspaceEnv::for_path(path);
        let (module, _) = crate::diagnostics::analyze_document(&doc, Some(&env));
        let pos = Position::new(10, 28); // at "rewrite this.num = a"
        let res = compute_completions(&doc, pos, &module, &env);
        match res {
            CompletionResponse::Array(items) => {
                let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
                println!("labels at 'rewrite this.num = a': {:?}", labels);
                assert!(labels.contains(&"abs_int".to_string()), "abs_int should be in completions, got: {:?}", labels);
            }
            _ => panic!("Expected array response"),
        }
    }

    #[test]
    fn test_class_extends_and_init_positions() {
        assert!(is_after_class_name("class MyClass ext"));
        assert!(!is_after_class_name("class MyClass"));
        assert!(!is_after_class_name("class MyClass extends "));
        assert!(!is_after_class_name("class MyClass extends M"));

        assert!(is_extends_position("class MyClass extends "));
        assert!(is_extends_position("class MyClass extends M"));
        assert!(is_extends_position("public class MyClass extends "));
        assert!(!is_extends_position("class MyClass "));

        assert!(is_init_position("init "));
        assert!(is_init_position("rewrite this.theme = init "));
        assert!(is_init_position("set x = init Sh"));
        assert!(!is_init_position("fun init()"));
    }

    #[test]
    fn test_snippets_formatting() {
        let (fn_snip, fmt) = function_signature_to_snippet("log_init", "fun log_init(file_path: string = \"log.txt\")");
        assert_eq!(fmt, Some(InsertTextFormat::SNIPPET));
        assert_eq!(fn_snip, "log_init(${1:file_path: string})");

        let (st_snip, fmt2) = struct_signature_to_snippet("Point", "struct Point { x: int32, y: int32 }");
        assert_eq!(fmt2, Some(InsertTextFormat::SNIPPET));
        assert!(st_snip.contains("x: ${1:int32}"));
        assert!(st_snip.contains("y: ${2:int32}"));

        let (cls_init, _) = class_snippet("ShowcaseTheme", "set x = ");
        assert_eq!(cls_init, "init ShowcaseTheme()$0");

        let (cls_direct, _) = class_snippet("ShowcaseTheme", "init ");
        assert_eq!(cls_direct, "ShowcaseTheme()$0");

        let dummy_span = yukic_span::Span::new(yukic_span::FileId(0), yukic_span::BytePos(0), yukic_span::BytePos(1));
        let param = yukic_ast::Param {
            name: yukic_ast::Ident::new("a", dummy_span),
            is_ref: false,
            is_mut: false,
            is_variadic: false,
            ty: Some(yukic_ast::TypeNode::new(
                yukic_ast::TypeNodeKind::Ident(yukic_ast::Ident::new("string", dummy_span)),
                dummy_span,
            )),
            default_val: None,
            span: dummy_span,
        };
        let (cls_with_arg, _) = class_snippet_with_params("Test", &[param], "set x = ");
        assert_eq!(cls_with_arg, "init Test(${1:a: string})$0");
    }

    #[test]
    fn test_this_dot_completion() {
        let path = Path::new("/home/shareui/dev/yuki-lang/source/offlibs/siesta/apps/demosecond/src/views.ykn");
        let uri = tower_lsp::lsp_types::Url::from_file_path(path).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        let doc = Document::new(uri, content, 1);
        let env = WorkspaceEnv::for_path(path);
        let (module, _) = crate::diagnostics::analyze_document(&doc, Some(&env));
        let mut items = Vec::new();
        populate_instance_dot_completions(&mut items, "this", &module, path, &env, &doc.text);
        let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
        println!("labels for this.: {:?}", labels);
        assert!(labels.contains(&"screen_idx".to_string()));
        assert!(labels.contains(&"theme".to_string()));
        assert!(labels.contains(&"overview_screen".to_string()));
        assert!(labels.contains(&"next_screen".to_string()));
    }

    #[test]
    fn test_class_extends_completion_suggestions() {
        let text = "class Base1 {}\nlocked class LockedBase {}\nclass Test1 extends \n";
        let uri = tower_lsp::lsp_types::Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let env = WorkspaceEnv::for_path(Path::new("."));
        let (module, _) = crate::diagnostics::analyze_document(&doc, Some(&env));
        let pos = Position::new(2, 20); // right after "class Test1 extends "
        let res = compute_completions(&doc, pos, &module, &env);

        match res {
            CompletionResponse::Array(items) => {
                let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
                assert!(labels.contains(&"Base1".to_string()), "Base1 should be suggested: {:?}", labels);
                assert!(!labels.contains(&"Test1".to_string()), "Current class Test1 must not be suggested: {:?}", labels);
                assert!(!labels.contains(&"LockedBase".to_string()), "Locked class must not be suggested: {:?}", labels);
                assert!(!labels.contains(&"extends".to_string()), "extends keyword must not be suggested again: {:?}", labels);
                assert!(!labels.contains(&"fun".to_string()), "keywords must not be suggested in extends position: {:?}", labels);
            }
            _ => panic!("Expected array completion response"),
        }
    }

    #[test]
    fn test_rewrite_target_position() {
        assert!(is_rewrite_target_position("rewrite "));
        assert!(is_rewrite_target_position("rewrite th"));
        assert!(!is_rewrite_target_position("rewrite this."));
        assert!(!is_rewrite_target_position("rewrite StructName."));
        assert!(!is_rewrite_target_position("rewrite this.name = "));
        assert!(!is_rewrite_target_position("set x = "));
    }

    #[test]
    fn test_rewrite_completion_inside_and_outside_class() {
        let text = "fun outside() {\n    set count = 0\n    rewrite \n}\n\nclass Counter {\n    val: int32\n    fun inside() {\n        set local_var = 1\n        rewrite \n    }\n}\n";
        let uri = tower_lsp::lsp_types::Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let env = WorkspaceEnv::for_path(Path::new("."));
        let (module, _) = crate::diagnostics::analyze_document(&doc, Some(&env));

        // line 2: inside function outside()
        let pos_outside = Position::new(2, 12);
        let res_outside = compute_completions(&doc, pos_outside, &module, &env);
        match res_outside {
            CompletionResponse::Array(items) => {
                let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
                assert!(labels.contains(&"count".to_string()), "count should be suggested: {:?}", labels);
                assert!(!labels.contains(&"this".to_string()), "this must NOT be suggested outside class: {:?}", labels);
                assert!(!labels.contains(&"this.val".to_string()), "this.val must NOT be suggested outside class: {:?}", labels);
            }
            _ => panic!("Expected array response"),
        }

        // line 9: inside class method inside()
        let pos_inside = Position::new(9, 16);
        let res_inside = compute_completions(&doc, pos_inside, &module, &env);
        match res_inside {
            CompletionResponse::Array(items) => {
                let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
                assert!(labels.contains(&"local_var".to_string()), "local_var should be suggested: {:?}", labels);
                assert!(labels.contains(&"this.val".to_string()), "this.val should be suggested inside class: {:?}", labels);
                assert!(labels.contains(&"this".to_string()), "this should be suggested inside class: {:?}", labels);
            }
            _ => panic!("Expected array response"),
        }
    }

    #[test]
    fn test_load_as_completion_suppression() {
        assert!(is_identifier_naming_position("load std.math: abs as "));
        assert!(is_identifier_naming_position("load std.math: abs as my_"));

        let path = Path::new("/home/shareui/dev/yuki-lang/source/exm/src/main.ykn");
        let uri = tower_lsp::lsp_types::Url::from_file_path(path).unwrap();
        let text = "load std.math: abs as ";
        let doc = Document::new(uri, text.to_string(), 1);
        let env = WorkspaceEnv::for_path(path);
        let (module, _) = crate::diagnostics::analyze_document(&doc, Some(&env));
        let pos = Position::new(0, 22);
        let res = compute_completions(&doc, pos, &module, &env);
        match res {
            CompletionResponse::Array(items) => {
                assert!(items.is_empty(), "Completions must be suppressed after 'as ', got: {:?}", items);
            }
            _ => panic!("Expected array response"),
        }
    }

    #[test]
    fn test_load_relative_and_brace_completion() {
        let path = Path::new("/home/shareui/dev/yuki-lang/source/exm/src/main.ykn");
        let uri = tower_lsp::lsp_types::Url::from_file_path(path).unwrap();
        let env = WorkspaceEnv::for_path(path);

        // "load ." -> should suggest "utils"
        let doc1 = Document::new(uri.clone(), "load .".to_string(), 1);
        let (mod1, _) = crate::diagnostics::analyze_document(&doc1, Some(&env));
        let res1 = compute_completions(&doc1, Position::new(0, 6), &mod1, &env);
        if let CompletionResponse::Array(items) = res1 {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"utils"), "load . should suggest 'utils', got: {:?}", labels);
        } else {
            panic!("Expected array");
        }

        // "load .{}" -> should suggest "utils"
        let doc2 = Document::new(uri.clone(), "load .{}".to_string(), 1);
        let (mod2, _) = crate::diagnostics::analyze_document(&doc2, Some(&env));
        let res2 = compute_completions(&doc2, Position::new(0, 7), &mod2, &env);
        if let CompletionResponse::Array(items) = res2 {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"utils"), "load .{{}} should suggest 'utils', got: {:?}", labels);
        } else {
            panic!("Expected array");
        }

        // "load .utils.{}" -> should suggest "test"
        let doc3 = Document::new(uri.clone(), "load .utils.{}".to_string(), 1);
        let (mod3, _) = crate::diagnostics::analyze_document(&doc3, Some(&env));
        let res3 = compute_completions(&doc3, Position::new(0, 13), &mod3, &env);
        if let CompletionResponse::Array(items) = res3 {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"test"), "load .utils.{{}} should suggest 'test', got: {:?}", labels);
        } else {
            panic!("Expected array");
        }
    }

    #[test]
    fn test_self_suggestion_suppressed() {
        let text = "fun my_func() {\n    \n}\n\nclass MyClass {\n    fun my_method() {\n        \n    }\n}\n";
        let uri = tower_lsp::lsp_types::Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let env = WorkspaceEnv::for_path(Path::new("."));
        let (module, _) = crate::diagnostics::analyze_document(&doc, Some(&env));

        // inside my_func: my_func should not be suggested
        let pos_fn = Position::new(1, 4);
        let res_fn = compute_completions(&doc, pos_fn, &module, &env);
        if let CompletionResponse::Array(items) = res_fn {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(!labels.contains(&"my_func"), "my_func should not suggest itself inside its body: {:?}", labels);
        }

        // inside MyClass.my_method: MyClass and my_method should not suggest themselves
        let pos_method = Position::new(6, 8);
        let res_method = compute_completions(&doc, pos_method, &module, &env);
        if let CompletionResponse::Array(items) = res_method {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(!labels.contains(&"my_method"), "my_method should not suggest itself inside its body: {:?}", labels);
            assert!(!labels.contains(&"MyClass"), "MyClass should not suggest itself inside its body: {:?}", labels);
        }
    }

    #[test]
    fn test_exact_user_scenarios() {
        let path = Path::new("/home/shareui/dev/yuki-lang/source/exm/src/main.ykn");
        let uri = tower_lsp::lsp_types::Url::from_file_path(path).unwrap();
        let env = WorkspaceEnv::for_path(path);

        let text1 = "load .utils.test: Test\nload core.utils: cast\n\nstruct Example {\n    inpt: int64\n    out: string\n}\n\nfun main() {\n    set st = Example {\n        inpt: -132,\n        out: \"\",\n    }\n\n    set exp = Test(st.\n";
        let doc1 = Document::new(uri.clone(), text1.to_string(), 1);
        let (mod1, _) = crate::diagnostics::analyze_document(&doc1, Some(&env));
        let res1 = compute_completions(&doc1, Position::new(14, 22), &mod1, &env);
        if let CompletionResponse::Array(items) = res1 {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"inpt"), "st. should suggest 'inpt', got: {:?}", labels);
            assert!(labels.contains(&"out"), "st. should suggest 'out', got: {:?}", labels);
            assert!(!labels.contains(&"abs"), "st. must NOT suggest 'abs' from Test: {:?}", labels);
            assert!(!labels.contains(&"num"), "st. must NOT suggest 'num' from Test: {:?}", labels);
        } else {
            panic!("Expected array response");
        }

        let text2 = "load .utils.test: Test\nload core.utils: cast\n\nstruct Example {\n    inpt: int64\n    out: string\n}\n\nfun main() {\n    set st = Example {\n        inpt: -132,\n        out: \"\",\n    }\n\n    set exp = Test(st.inpt)\n\n    rewrite st.out = cast(exp.\n";
        let doc2 = Document::new(uri.clone(), text2.to_string(), 1);
        let (mod2, _) = crate::diagnostics::analyze_document(&doc2, Some(&env));
        let res2 = compute_completions(&doc2, Position::new(16, 30), &mod2, &env);
        if let CompletionResponse::Array(items) = res2 {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"abs"), "exp. should suggest 'abs', got: {:?}", labels);
            assert!(labels.contains(&"num"), "exp. should suggest 'num', got: {:?}", labels);
            assert!(!labels.contains(&"inpt"), "exp. must NOT suggest 'inpt' from Example: {:?}", labels);
        } else {
            panic!("Expected array response");
        }

        let text3 = "load .utils.test: Test\nload core.utils: cast\n\nstruct Example {\n    inpt: int64\n    out: string\n}\n\nfun main() {\n    set st = Example {\n        inpt: -132,\n        out: \"\",\n    }\n\n    set exp = Test(\n";
        let doc3 = Document::new(uri.clone(), text3.to_string(), 1);
        let (mod3, _) = crate::diagnostics::analyze_document(&doc3, Some(&env));
        let res3 = compute_completions(&doc3, Position::new(14, 19), &mod3, &env);
        if let CompletionResponse::Array(items) = res3 {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"st"), "in-scope variable 'st' should be suggested inside Test(..): {:?}", labels);
            assert!(!labels.contains(&"core"), "bare package 'core' should NOT be suggested in expressions: {:?}", labels);
            assert!(!labels.contains(&"siesta"), "bare package 'siesta' should NOT be suggested in expressions: {:?}", labels);
        } else {
            panic!("Expected array response");
        }

        let text4 = "struct Example {\n    inpt: int64\n    out: string\n}\n\nfun main() {\n    set st = 123\n    rewrite Example.\n";
        let doc4 = Document::new(uri.clone(), text4.to_string(), 1);
        let (mod4, _) = crate::diagnostics::analyze_document(&doc4, Some(&env));
        let res4 = compute_completions(&doc4, Position::new(7, 20), &mod4, &env);
        if let CompletionResponse::Array(items) = res4 {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"inpt"), "rewrite Example. should suggest 'inpt', got: {:?}", labels);
            assert!(labels.contains(&"out"), "rewrite Example. should suggest 'out', got: {:?}", labels);
            assert!(!labels.contains(&"st"), "rewrite Example. must NOT suggest variable 'st', got: {:?}", labels);
        } else {
            panic!("Expected array response");
        }

        let text5 = "struct Example {\n    inpt: int64\n    out: string\n}\n\nfun main() {\n    set st = Example {\n        inpt: -132,\n        out: \"\",\n    }\n    rewrite st.\n";
        let doc5 = Document::new(uri, text5.to_string(), 1);
        let (mod5, _) = crate::diagnostics::analyze_document(&doc5, Some(&env));
        let res5 = compute_completions(&doc5, Position::new(10, 15), &mod5, &env);
        if let CompletionResponse::Array(items) = res5 {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"inpt"), "rewrite st. should suggest 'inpt', got: {:?}", labels);
            assert!(labels.contains(&"out"), "rewrite st. should suggest 'out', got: {:?}", labels);
            assert!(!labels.contains(&"st"), "rewrite st. must NOT suggest variable 'st', got: {:?}", labels);
        } else {
            panic!("Expected array response");
        }
    }

    #[test]
    fn test_cload_snippet_and_header_path_completions() {
        let root = std::env::temp_dir().join(format!("yukino_lsp_cload_comp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("native/sub")).unwrap();
        std::fs::create_dir_all(root.join("yuki-work")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("yuki.toml"), "[package]\nname = \"cload_test\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::write(root.join("native/api.h"), "int c_add(int a, int b);\n").unwrap();
        std::fs::write(root.join("native/sub/math.h"), "int sub_mul(int x, int y);\n").unwrap();
        std::fs::write(root.join("yuki-work/ignored.h"), "int ignored_fn(void);\n").unwrap();
        std::fs::write(root.join("target/ignored.h"), "int ignored_fn2(void);\n").unwrap();

        let main_path = root.join("src/main.ykn");
        let uri = tower_lsp::lsp_types::Url::from_file_path(&main_path).unwrap();
        let env = WorkspaceEnv::for_path(&main_path);

        // 1. Top-level cload keyword snippet inserts cload "$0" and retriggers suggestions.
        let doc_kw = Document::new(uri.clone(), "cl\n".to_string(), 1);
        let (mod_kw, _) = crate::diagnostics::analyze_document(&doc_kw, Some(&env));
        let res_kw = compute_completions(&doc_kw, Position::new(0, 2), &mod_kw, &env);
        if let CompletionResponse::Array(items) = res_kw {
            let cload_item = items.iter().find(|i| i.label == "cload").expect("expected cload snippet");
            assert_eq!(cload_item.insert_text.as_deref(), Some("cload \"$0\""));
            assert_eq!(
                cload_item.command.as_ref().map(|c| c.command.as_str()),
                Some("editor.action.triggerSuggest")
            );
        } else {
            panic!("Expected array response");
        }

        // 2. cload "{cursor}" (closing quote exists -> must NOT append extra ").
        let doc_closed = Document::new(uri.clone(), "cload \"\"\n".to_string(), 1);
        let (mod_closed, _) = crate::diagnostics::analyze_document(&doc_closed, Some(&env));
        let res_closed = compute_completions(&doc_closed, Position::new(0, 7), &mod_closed, &env);
        if let CompletionResponse::Array(items) = res_closed {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"native/api.h"), "missing native/api.h: {:?}", labels);
            assert!(labels.contains(&"native/sub/math.h"), "missing native/sub/math.h: {:?}", labels);
            assert!(!labels.contains(&"yuki-work/ignored.h"), "must ignore yuki-work: {:?}", labels);
            assert!(!labels.contains(&"target/ignored.h"), "must ignore target: {:?}", labels);
            let api_item = items.iter().find(|i| i.label == "native/api.h").unwrap();
            match api_item.text_edit.as_ref().expect("expected text_edit") {
                CompletionTextEdit::Edit(te) => {
                    assert_eq!(te.new_text, "native/api.h");
                }
                _ => panic!("expected simple TextEdit"),
            }
        } else {
            panic!("Expected array response");
        }

        // 3. cload "{cursor} (missing closing quote -> must append ").
        let doc_open = Document::new(uri.clone(), "cload \"\n".to_string(), 1);
        let (mod_open, _) = crate::diagnostics::analyze_document(&doc_open, Some(&env));
        let res_open = compute_completions(&doc_open, Position::new(0, 7), &mod_open, &env);
        if let CompletionResponse::Array(items) = res_open {
            let api_item = items.iter().find(|i| i.label == "native/api.h").unwrap();
            match api_item.text_edit.as_ref().expect("expected text_edit") {
                CompletionTextEdit::Edit(te) => {
                    assert_eq!(te.new_text, "native/api.h\"");
                }
                _ => panic!("expected simple TextEdit"),
            }
        } else {
            panic!("Expected array response");
        }

        // 4. load "{cursor} and load "{cursor}" trigger .h completions with conditional closing quote.
        let doc_load_open = Document::new(uri.clone(), "load \"\n".to_string(), 1);
        let (mod_load_open, _) = crate::diagnostics::analyze_document(&doc_load_open, Some(&env));
        if let CompletionResponse::Array(items) = compute_completions(&doc_load_open, Position::new(0, 6), &mod_load_open, &env) {
            let api_item = items.iter().find(|i| i.label == "native/api.h").unwrap();
            match api_item.text_edit.as_ref().unwrap() {
                CompletionTextEdit::Edit(te) => assert_eq!(te.new_text, "native/api.h\""),
                _ => panic!("expected simple TextEdit"),
            }
        } else {
            panic!("Expected array response");
        }

        // 5. Partial prefix cload "native/sub{cursor}" filters and replaces full relative range.
        let doc_partial = Document::new(uri, "cload \"native/sub\"\n".to_string(), 1);
        let (mod_partial, _) = crate::diagnostics::analyze_document(&doc_partial, Some(&env));
        if let CompletionResponse::Array(items) = compute_completions(&doc_partial, Position::new(0, 17), &mod_partial, &env) {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert_eq!(labels, vec!["native/sub/math.h"]);
            match items[0].text_edit.as_ref().unwrap() {
                CompletionTextEdit::Edit(te) => {
                    assert_eq!(te.range.start.character, 7);
                    assert_eq!(te.range.end.character, 17);
                    assert_eq!(te.new_text, "native/sub/math.h");
                }
                _ => panic!("expected simple TextEdit"),
            }
        } else {
            panic!("Expected array response");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_cload_symbol_namespace_and_cstruct_dot_completion() {
        let root = std::env::temp_dir().join(format!("yukino_lsp_cload_sym_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("native")).unwrap();
        std::fs::write(root.join("yuki.toml"), "[package]\nname = \"cload_sym\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::write(
            root.join("native/api.h"),
            "#define API_VERSION 2\ntypedef struct Packet {\n    int id;\n    int tag;\n} Packet;\ntypedef union ValueUnion {\n    int i;\n    double f;\n} ValueUnion;\nextern int global_counter;\nint c_add(int a, int b);\n",
        )
        .unwrap();

        let main_path = root.join("src/main.ykn");
        let uri = tower_lsp::lsp_types::Url::from_file_path(&main_path).unwrap();
        let env = WorkspaceEnv::for_path(&main_path);

        // Expression completion includes header symbols + header namespace.
        let text1 = "cload \"native/api.h\"\nfun main() {\n    set x = \n}\n";
        let doc1 = Document::new(uri.clone(), text1.to_string(), 1);
        let (mod1, _) = crate::diagnostics::analyze_document(&doc1, Some(&env));
        if let CompletionResponse::Array(items) = compute_completions(&doc1, Position::new(2, 12), &mod1, &env) {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            for expected in ["c_add", "Packet", "ValueUnion", "global_counter", "API_VERSION", "api"] {
                assert!(labels.contains(&expected), "missing '{expected}' in completions: {:?}", labels);
            }
        } else {
            panic!("Expected array response");
        }

        // Scoped completion after api::
        let text2 = "cload \"native/api.h\"\nfun main() {\n    set x = api::\n}\n";
        let doc2 = Document::new(uri.clone(), text2.to_string(), 1);
        let (mod2, _) = crate::diagnostics::analyze_document(&doc2, Some(&env));
        if let CompletionResponse::Array(items) = compute_completions(&doc2, Position::new(2, 17), &mod2, &env) {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"c_add"), "api:: should suggest c_add, got: {:?}", labels);
            assert!(labels.contains(&"Packet"), "api:: should suggest Packet, got: {:?}", labels);
        } else {
            panic!("Expected array response");
        }

        // Dot completion on C struct from cload header
        let text3 = "cload \"native/api.h\"\nfun main() {\n    set p = Packet { id: 1, tag: 2 }\n    set y = p.\n}\n";
        let doc3 = Document::new(uri, text3.to_string(), 1);
        let (mod3, _) = crate::diagnostics::analyze_document(&doc3, Some(&env));
        if let CompletionResponse::Array(items) = compute_completions(&doc3, Position::new(3, 14), &mod3, &env) {
            let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(labels.contains(&"id"), "p. should suggest 'id', got: {:?}", labels);
            assert!(labels.contains(&"tag"), "p. should suggest 'tag', got: {:?}", labels);
        } else {
            panic!("Expected array response");
        }

        let _ = std::fs::remove_dir_all(&root);
    }
}
