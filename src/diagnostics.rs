use std::collections::HashSet;
use std::path::Path;
use tower_lsp::lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticSeverity as LspSeverity, NumberOrString, Range,
};
use yukic_ast::*;
use yukic_diagnostics::{Diagnostic, DiagnosticSeverity};
use yukic_lexer::Lexer;
use yukic_parser::Parser;
use yukic_span::{BytePos, FileId};

use crate::document::Document;
use crate::workspace::WorkspaceEnv;

pub fn analyze_document(
    doc: &Document,
    env: Option<&WorkspaceEnv>,
) -> (Module, Vec<LspDiagnostic>) {
    let file_id = FileId(0);
    let lexer = Lexer::new(file_id, &doc.text);
    let (tokens, mut diags) = lexer.tokenize();

    let mut parser = Parser::with_file_id(tokens, file_id);
    let (module, mut parser_diags) = parser.parse_module();
    diags.append(&mut parser_diags);

    let mut lsp_diags: Vec<LspDiagnostic> = diags
        .into_iter()
        .map(|diag| convert_to_lsp_diagnostic(doc, diag))
        .collect();

    let mut seen_parser_lines = HashSet::new();
    lsp_diags.retain(|d| {
        if let Some(NumberOrString::String(ref code)) = d.code {
            if code == "E0201" {
                return seen_parser_lines.insert(d.range.start.line);
            }
        }
        true
    });

    let current_file = doc.uri.to_file_path().unwrap_or_default();
    if let Some(workspace_env) = env {
        validate_imports(&module, doc, current_file.as_path(), workspace_env, &mut lsp_diags);
    }
    validate_numeric_bounds(&module, doc, &mut lsp_diags);
    validate_symbol_scopes(&module, doc, current_file.as_path(), env, &mut lsp_diags);
    crate::type_check::validate_types_and_calls(&module, doc, current_file.as_path(), env, &mut lsp_diags);
    crate::lint::run_lint_checks(&module, doc, &mut lsp_diags);

    let mut seen_keys = HashSet::new();
    lsp_diags.retain(|d| {
        let key = (d.range.start.line, d.range.start.character, d.message.clone());
        seen_keys.insert(key)
    });

    (module, lsp_diags)
}

fn validate_imports(
    module: &Module,
    doc: &Document,
    current_file: &std::path::Path,
    env: &WorkspaceEnv,
    diags: &mut Vec<LspDiagnostic>,
) {
    for item in &module.items {
        if let ItemKind::Import(ref imp) = item.kind {
            let path_str = format_import_path(&imp.path);

            match env.resolve_import(&imp.path, current_file) {
                None => {
                    let range = doc.span_to_range(imp.path.span.start, imp.path.span.end);
                    diags.push(LspDiagnostic {
                        range,
                        severity: Some(LspSeverity::ERROR),
                        code: Some(NumberOrString::String("E0301".to_string())),
                        code_description: None,
                        source: Some("yukino".to_string()),
                        message: format!("Cannot find module '{path_str}'"),
                        related_information: None,
                        tags: None,
                        data: None,
                    });
                }
                Some(target_path) => {
                    if let Some(ref clause) = imp.symbols {
                        if let Some(cached) = env.get_or_parse_module(&target_path) {
                            for sym_item in &clause.items {
                                if let ImportPattern::Exact(ref id) = sym_item.pattern {
                                    let exists = cached.exports.iter().any(|exp| exp.name == id.name);
                                    if !exists {
                                        let range = doc.span_to_range(id.span.start, id.span.end);
                                        diags.push(LspDiagnostic {
                                            range,
                                            severity: Some(LspSeverity::ERROR),
                                            code: Some(NumberOrString::String("E0303".to_string())),
                                            code_description: None,
                                            source: Some("yukino".to_string()),
                                            message: format!(
                                                "Symbol '{}' is not exported by module '{path_str}'",
                                                id.name
                                            ),
                                            related_information: None,
                                            tags: None,
                                            data: None,
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
}

fn format_import_path(path: &ImportPath) -> String {
    let mut s = String::new();
    match &path.prefix {
        yukic_ast::PathPrefix::Relative(n) => {
            for _ in 0..=*n {
                s.push('.');
            }
        }
        yukic_ast::PathPrefix::Root => {
            s.push_str("root.");
        }
        yukic_ast::PathPrefix::Package(id) => {
            s.push_str(&id.name);
        }
    }
    for seg in &path.segments {
        if !s.is_empty() && !s.ends_with('.') {
            s.push('.');
        }
        match &seg.pattern {
            ImportPattern::Exact(id) => s.push_str(&id.name),
            ImportPattern::All => s.push('*'),
            ImportPattern::Prefix(id) => {
                s.push_str(&id.name);
                s.push('*');
            }
            ImportPattern::Suffix(id) => {
                s.push('*');
                s.push_str(&id.name);
            }
        }
    }
    s
}

fn convert_to_lsp_diagnostic(doc: &Document, diag: Diagnostic) -> LspDiagnostic {
    let range = resolve_diagnostic_range(doc, &diag);
    let severity = match diag.severity {
        DiagnosticSeverity::Error => Some(LspSeverity::ERROR),
        DiagnosticSeverity::Warning => Some(LspSeverity::WARNING),
        DiagnosticSeverity::Note => Some(LspSeverity::INFORMATION),
        DiagnosticSeverity::Help => Some(LspSeverity::HINT),
    };

    let mut full_message = diag.message.clone();
    for note in &diag.notes {
        full_message.push_str("\nNote: ");
        full_message.push_str(note);
    }
    for suggestion in &diag.suggestions {
        full_message.push_str("\nHelp: ");
        full_message.push_str(&suggestion.description);
        full_message.push_str(" -> `");
        full_message.push_str(&suggestion.replacement);
        full_message.push_str("`");
    }

    LspDiagnostic {
        range,
        severity,
        code: Some(NumberOrString::String(diag.code.to_string())),
        code_description: None,
        source: Some("yukino".to_string()),
        message: full_message,
        related_information: None,
        tags: None,
        data: None,
    }
}

fn resolve_diagnostic_range(doc: &Document, diag: &Diagnostic) -> Range {
    if let Some(span) = diag.primary_span {
        if !span.is_dummy() {
            return doc.span_to_range(span.start, span.end);
        }
    }

    for label in &diag.labels {
        if !label.span.is_dummy() {
            return doc.span_to_range(label.span.start, label.span.end);
        }
    }

    doc.span_to_range(BytePos(0), BytePos(0))
}

fn validate_numeric_bounds(
    module: &Module,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) => {
                check_block_numeric_bounds(&f.body, doc, diags);
            }
            ItemKind::Class(ref c) => {
                for m in &c.methods {
                    check_block_numeric_bounds(&m.body, doc, diags);
                }
            }
            ItemKind::Struct(ref s) => {
                for m in &s.methods {
                    check_block_numeric_bounds(&m.body, doc, diags);
                }
            }
            ItemKind::Static(ref s) => {
                if let Some(ref ty) = s.ty {
                    check_numeric_bounds_expr(&s.init, &ty.to_string(), doc, diags);
                }
            }
            _ => {}
        }
    }
}

fn check_block_numeric_bounds(block: &Block, doc: &Document, diags: &mut Vec<LspDiagnostic>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Set { ty: Some(ref ty), init, .. } => {
                check_numeric_bounds_expr(init, &ty.to_string(), doc, diags);
            }
            StmtKind::Static(ref s) => {
                if let Some(ref ty) = s.ty {
                    check_numeric_bounds_expr(&s.init, &ty.to_string(), doc, diags);
                }
            }
            StmtKind::Expr(ref expr) => {
                check_expr_inner_blocks(expr, doc, diags);
            }
            StmtKind::Defer(ref b) => {
                check_block_numeric_bounds(b, doc, diags);
            }
            _ => {}
        }
    }
}

fn check_expr_inner_blocks(expr: &Expr, doc: &Document, diags: &mut Vec<LspDiagnostic>) {
    match &expr.kind {
        ExprKind::Block(ref b) | ExprKind::RegionBlock(ref b) => {
            check_block_numeric_bounds(b, doc, diags);
        }
        ExprKind::RegionScopedBlock(_, ref b) => {
            check_block_numeric_bounds(b, doc, diags);
        }
        ExprKind::If { then_branch, elif_branches, else_branch, .. } => {
            check_block_numeric_bounds(then_branch, doc, diags);
            for (_, elif_body) in elif_branches {
                check_block_numeric_bounds(elif_body, doc, diags);
            }
            if let Some(ref el) = else_branch {
                check_block_numeric_bounds(el, doc, diags);
            }
        }
        ExprKind::While { body, .. } | ExprKind::For { body, .. } | ExprKind::Loop { body, .. } => {
            check_block_numeric_bounds(body, doc, diags);
        }
        ExprKind::Sus { body, catches, finally_branch } => {
            check_block_numeric_bounds(body, doc, diags);
            for c in catches {
                check_block_numeric_bounds(&c.body, doc, diags);
            }
            if let Some(ref fin) = finally_branch {
                check_block_numeric_bounds(fin, doc, diags);
            }
        }
        _ => {}
    }
}

fn check_numeric_bounds_expr(
    init: &Expr,
    ty_str: &str,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let (min, max) = match ty_str {
        "int8" => (i8::MIN as i128, i8::MAX as i128),
        "int16" => (i16::MIN as i128, i16::MAX as i128),
        "int32" => (i32::MIN as i128, i32::MAX as i128),
        "int64" | "isize" => (i64::MIN as i128, i64::MAX as i128),
        "uint8" => (0i128, u8::MAX as i128),
        "uint16" => (0i128, u16::MAX as i128),
        "uint32" => (0i128, u32::MAX as i128),
        "uint64" | "usize" => (0i128, u64::MAX as i128),
        _ => return,
    };

    let slice = doc.slice_span(init.span.start, init.span.end).trim();
    let num_str = slice.trim_end_matches(|c: char| c.is_alphabetic() || c == '_');

    let parsed = if num_str.starts_with("0x") || num_str.starts_with("0X") {
        i128::from_str_radix(&num_str[2..], 16).ok()
    } else if num_str.starts_with("0b") || num_str.starts_with("0B") {
        i128::from_str_radix(&num_str[2..], 2).ok()
    } else if num_str.starts_with("0o") || num_str.starts_with("0O") {
        i128::from_str_radix(&num_str[2..], 8).ok()
    } else {
        num_str.parse::<i128>().ok()
    };

    let out_of_range = match parsed {
        Some(val) => val < min || val > max,
        None if !num_str.is_empty() && (num_str.chars().all(|c| c.is_ascii_digit()) || (num_str.starts_with('-') && num_str[1..].chars().all(|c| c.is_ascii_digit()))) => true,
        _ => false,
    };

    if out_of_range {
        let range = doc.span_to_range(init.span.start, init.span.end);
        diags.retain(|d| !(d.range == range && d.message.contains("64-bit")));

        diags.push(LspDiagnostic {
            range,
            severity: Some(LspSeverity::ERROR),
            code: Some(NumberOrString::String("E0109".to_string())),
            code_description: None,
            source: Some("yukino".to_string()),
            message: format!("Literal value `{slice}` is out of range for type `{ty_str}` (allowed: {min}..={max})"),
            related_information: None,
            tags: None,
            data: None,
        });
    }
}

fn validate_symbol_scopes(
    module: &Module,
    doc: &Document,
    current_file: &Path,
    env: Option<&WorkspaceEnv>,
    diags: &mut Vec<LspDiagnostic>,
) {
    let global_symbols = collect_global_symbols(module, current_file, env);

    // E0304: Duplicate declarations in same module
    let mut declared_symbols: std::collections::HashMap<String, yukic_span::Span> = std::collections::HashMap::new();
    for item in &module.items {
        let (name, span) = match &item.kind {
            ItemKind::Fun(f) => (Some(&f.name.name), f.name.span),
            ItemKind::Class(c) => (Some(&c.name.name), c.name.span),
            ItemKind::Struct(s) => (Some(&s.name.name), s.name.span),
            ItemKind::Interface(i) => (Some(&i.name.name), i.name.span),
            ItemKind::Enum(e) => (Some(&e.name.name), e.name.span),
            ItemKind::Alias(a) => (Some(&a.alias.name), a.alias.span),
            _ => (None, yukic_span::Span::new(FileId(0), BytePos(0), BytePos(0))),
        };
        if let Some(n) = name {
            if declared_symbols.insert(n.clone(), span).is_some() {
                let range = doc.span_to_range(span.start, span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0304".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: format!("Symbol '{n}' is already defined in this module"),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
        }
    }

    // E0409: Cannot subclass locked class
    for item in &module.items {
        if let ItemKind::Class(ref cls) = item.kind {
            if let Some(ref parent_id) = cls.base_class {
                let is_locked = module.items.iter().any(|it| {
                    if let ItemKind::Class(ref c) = it.kind {
                        c.name.name == parent_id.name && c.is_locked
                    } else {
                        false
                    }
                });
                if is_locked {
                    let range = doc.span_to_range(parent_id.span.start, parent_id.span.end);
                    diags.push(LspDiagnostic {
                        range,
                        severity: Some(LspSeverity::ERROR),
                        code: Some(NumberOrString::String("E0409".to_string())),
                        code_description: None,
                        source: Some("yukino".to_string()),
                        message: format!("Cannot subclass locked class '{}'", parent_id.name),
                        related_information: None,
                        tags: None,
                        data: None,
                    });
                }
            }
        }
    }

    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) => {
                let mut scopes = vec![HashSet::new()];
                for p in &f.params {
                    scopes[0].insert(p.name.name.clone());
                }
                validate_block(&f.body, &mut scopes, &global_symbols, doc, diags);
            }
            ItemKind::Class(ref cls) => {
                let has_parent = cls.base_class.is_some();
                for m in &cls.methods {
                    let mut scopes = vec![HashSet::new()];
                    scopes[0].insert("this".to_string());
                    if has_parent {
                        scopes[0].insert("super".to_string());
                    }
                    for p in &m.params {
                        scopes[0].insert(p.name.name.clone());
                    }
                    validate_block(&m.body, &mut scopes, &global_symbols, doc, diags);
                }
                for a in &cls.accessors {
                    let mut scopes = vec![HashSet::new()];
                    scopes[0].insert("this".to_string());
                    if has_parent {
                        scopes[0].insert("super".to_string());
                    }
                    validate_block(&a.body, &mut scopes, &global_symbols, doc, diags);
                }
            }
            ItemKind::Struct(ref s) => {
                for m in &s.methods {
                    let mut scopes = vec![HashSet::new()];
                    scopes[0].insert("this".to_string());
                    for p in &m.params {
                        scopes[0].insert(p.name.name.clone());
                    }
                    validate_block(&m.body, &mut scopes, &global_symbols, doc, diags);
                }
            }
            _ => {}
        }
    }
}

fn collect_global_symbols(
    module: &Module,
    current_file: &Path,
    env: Option<&WorkspaceEnv>,
) -> HashSet<String> {
    let mut symbols = HashSet::new();

    let builtins = [
        "null", "space", "true", "false", "errkind",
        "cast", "panic", "typeof!", "args", "raw_args", "exit", "assert",
        "ptr_of", "from_raw_parts", "file!", "line!", "column!", "module_path!",
        "cfg!", "include_str!", "include_bytes!",
        "int8", "int16", "int32", "int64", "uint8", "uint16", "uint32", "uint64",
        "float32", "float64", "bool", "char", "string", "bytes", "void", "never",
        "usize", "isize",
    ];
    for b in builtins {
        symbols.insert(b.to_string());
    }

    if let Some(workspace_env) = env {
        for lib in &workspace_env.available_libs {
            symbols.insert(lib.clone());
        }
    }
    symbols.insert("std".to_string());
    symbols.insert("core".to_string());
    symbols.insert("root".to_string());

    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) => { symbols.insert(f.name.name.clone()); }
            ItemKind::Cfun(ref c) => { symbols.insert(c.name.name.clone()); }
            ItemKind::Class(ref c) => { symbols.insert(c.name.name.clone()); }
            ItemKind::Struct(ref s) => { symbols.insert(s.name.name.clone()); }
            ItemKind::Cstruct(ref cs) => { symbols.insert(cs.name.name.clone()); }
            ItemKind::Interface(ref i) => { symbols.insert(i.name.name.clone()); }
            ItemKind::Enum(ref e) => {
                symbols.insert(e.name.name.clone());
                for v in &e.variants {
                    match v {
                        EnumVariant::Simple(ref id) => { symbols.insert(id.name.clone()); }
                        EnumVariant::WithValue(ref id, _) => { symbols.insert(id.name.clone()); }
                        EnumVariant::Adt(ref id, _) => { symbols.insert(id.name.clone()); }
                    }
                }
            }
            ItemKind::Static(ref s) => { symbols.insert(s.name.name.clone()); }
            ItemKind::Macro(ref m) => { symbols.insert(m.name.name.clone()); }
            ItemKind::TypeAlias(ref ta) => { symbols.insert(ta.name.name.clone()); }
            ItemKind::Alias(ref a) => { symbols.insert(a.alias.name.clone()); }
            ItemKind::Externc(ref ext) => {
                for ext_item in &ext.items {
                    match ext_item {
                        ExterncItem::Cfun(ref c) => { symbols.insert(c.name.name.clone()); }
                        ExterncItem::Cstruct(ref cs) => { symbols.insert(cs.name.name.clone()); }
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(workspace_env) = env {
        for item in &module.items {
            if let ItemKind::Import(ref imp) = item.kind {
                if let Some(target_file) = workspace_env.resolve_import(&imp.path, current_file) {
                    if let Some(cached) = workspace_env.get_or_parse_module(&target_file) {
                        match &imp.symbols {
                            None => {
                                for exp in &cached.exports {
                                    symbols.insert(exp.name.clone());
                                }
                            }
                            Some(clause) => {
                                for (idx, sym) in clause.items.iter().enumerate() {
                                    if let Some(alias) = clause.aliases.get(idx) {
                                        symbols.insert(alias.name.clone());
                                    }
                                    match &sym.pattern {
                                        ImportPattern::Exact(id) => {
                                            symbols.insert(id.name.clone());
                                        }
                                        ImportPattern::All => {
                                            for exp in &cached.exports {
                                                symbols.insert(exp.name.clone());
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(workspace_env) = env {
        for sym in &workspace_env.allow_undef {
            symbols.insert(sym.clone());
        }
    }

    if let Some(parent) = current_file.parent() {
        let spes_classes = crate::workspace::scan_spes_classes(parent);
        for cls in spes_classes {
            symbols.insert(cls);
        }
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() && p.extension().is_some_and(|e| e == "ykn") && p != current_file {
                    if let Ok(text) = std::fs::read_to_string(&p) {
                        for line in text.lines() {
                            let trimmed = line.trim();
                            for kw in &["class ", "struct ", "enum ", "public class ", "public struct "] {
                                if let Some(rest) = trimmed.strip_prefix(kw) {
                                    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                                    if !name.is_empty() {
                                        symbols.insert(name);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    symbols
}

fn validate_block(
    block: &Block,
    scopes: &mut Vec<HashSet<String>>,
    globals: &HashSet<String>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    scopes.push(HashSet::new());
    for stmt in &block.stmts {
        validate_stmt(stmt, scopes, globals, doc, diags);
    }
    scopes.pop();
}

fn validate_stmt(
    stmt: &Stmt,
    scopes: &mut Vec<HashSet<String>>,
    globals: &HashSet<String>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    match &stmt.kind {
        StmtKind::Set { target, init, .. } => {
            validate_expr(init, scopes, globals, doc, diags);
            bind_target(target, scopes);
        }
        StmtKind::Static(ref s) => {
            validate_expr(&s.init, scopes, globals, doc, diags);
            if let Some(scope) = scopes.last_mut() {
                scope.insert(s.name.name.clone());
            }
        }
        StmtKind::Rewrite { target, value, .. } => {
            validate_lvalue(target, scopes, globals, doc, diags);
            validate_expr(value, scopes, globals, doc, diags);
        }
        StmtKind::Expr(ref expr) => {
            validate_expr(expr, scopes, globals, doc, diags);
        }
        StmtKind::Return(Some(ref expr)) => {
            validate_expr(expr, scopes, globals, doc, diags);
        }
        StmtKind::Recur(ref args) => {
            for a in args {
                validate_expr(a, scopes, globals, doc, diags);
            }
        }
        StmtKind::Defer(ref b) => {
            validate_block(b, scopes, globals, doc, diags);
        }
        _ => {}
    }
}

fn validate_expr(
    expr: &Expr,
    scopes: &mut Vec<HashSet<String>>,
    globals: &HashSet<String>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    match &expr.kind {
        ExprKind::This => {
            if !is_in_scope("this", scopes, globals) {
                let range = doc.span_to_range(expr.span.start, expr.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0305".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: "Cannot find symbol 'this' in this scope".to_string(),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
        }
        ExprKind::Super => {
            if !is_in_scope("super", scopes, globals) {
                let range = doc.span_to_range(expr.span.start, expr.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0416".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: "Cannot use 'super' here: class has no superclass".to_string(),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
        }
        ExprKind::Ident(ref id) => {
            if id.name == "super" && !is_in_scope("super", scopes, globals) {
                let range = doc.span_to_range(id.span.start, id.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0416".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: "Cannot use 'super' here: class has no superclass".to_string(),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            } else if !is_in_scope(&id.name, scopes, globals) {
                let range = doc.span_to_range(id.span.start, id.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0305".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: format!("Cannot find symbol '{}' in this scope", id.name),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
        }
        ExprKind::FieldAccess(ref obj, _) => {
            validate_expr(obj, scopes, globals, doc, diags);
        }
        ExprKind::MethodCall(ref obj, _, ref args) => {
            validate_expr(obj, scopes, globals, doc, diags);
            for a in args {
                validate_expr(a, scopes, globals, doc, diags);
            }
        }
        ExprKind::SafeCall(ref obj, _, ref args) => {
            validate_expr(obj, scopes, globals, doc, diags);
            for a in args {
                validate_expr(a, scopes, globals, doc, diags);
            }
        }
        ExprKind::Call(ref callee, ref args) => {
            validate_expr(callee, scopes, globals, doc, diags);
            for a in args {
                validate_expr(a, scopes, globals, doc, diags);
            }
        }
        ExprKind::ClassInit(ref callee, ref args) => {
            validate_expr(callee, scopes, globals, doc, diags);
            for a in args {
                validate_expr(a, scopes, globals, doc, diags);
            }
        }
        ExprKind::StructInit(ref name, ref fields) => {
            if !is_in_scope(&name.name, scopes, globals) {
                let range = doc.span_to_range(name.span.start, name.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0305".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: format!("Cannot find struct '{}' in this scope", name.name),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
            for f in fields {
                validate_expr(&f.value, scopes, globals, doc, diags);
            }
        }
        ExprKind::Binary(_, ref l, ref r) => {
            validate_expr(l, scopes, globals, doc, diags);
            validate_expr(r, scopes, globals, doc, diags);
        }
        ExprKind::Unary(_, ref inner) => {
            validate_expr(inner, scopes, globals, doc, diags);
        }
        ExprKind::Ternary(ref a, ref b, ref c) => {
            validate_expr(a, scopes, globals, doc, diags);
            validate_expr(b, scopes, globals, doc, diags);
            validate_expr(c, scopes, globals, doc, diags);
        }
        ExprKind::ArrayInit(ref items) | ExprKind::TupleInit(ref items) => {
            for it in items {
                validate_expr(it, scopes, globals, doc, diags);
            }
        }
        ExprKind::MapInit(ref pairs) => {
            for (k, v) in pairs {
                validate_expr(k, scopes, globals, doc, diags);
                validate_expr(v, scopes, globals, doc, diags);
            }
        }
        ExprKind::IndexAccess(ref obj, ref idx) => {
            validate_expr(obj, scopes, globals, doc, diags);
            validate_expr(idx, scopes, globals, doc, diags);
        }
        ExprKind::SliceAccess(ref obj, ref s, ref e, _) => {
            validate_expr(obj, scopes, globals, doc, diags);
            if let Some(ref st) = s {
                validate_expr(st, scopes, globals, doc, diags);
            }
            if let Some(ref ed) = e {
                validate_expr(ed, scopes, globals, doc, diags);
            }
        }
        ExprKind::Block(ref b) | ExprKind::RegionBlock(ref b) => {
            validate_block(b, scopes, globals, doc, diags);
        }
        ExprKind::RegionScopedBlock(ref reg, ref b) => {
            validate_expr(reg, scopes, globals, doc, diags);
            validate_block(b, scopes, globals, doc, diags);
        }
        ExprKind::If {
            cond,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            validate_expr(cond, scopes, globals, doc, diags);
            validate_block(then_branch, scopes, globals, doc, diags);
            for (elif_cond, elif_body) in elif_branches {
                validate_expr(elif_cond, scopes, globals, doc, diags);
                validate_block(elif_body, scopes, globals, doc, diags);
            }
            if let Some(ref el) = else_branch {
                validate_block(el, scopes, globals, doc, diags);
            }
        }
        ExprKind::While { cond, body, .. } => {
            validate_expr(cond, scopes, globals, doc, diags);
            validate_block(body, scopes, globals, doc, diags);
        }
        ExprKind::For {
            item_var,
            index_var,
            iter,
            body,
            ..
        } => {
            validate_expr(iter, scopes, globals, doc, diags);
            scopes.push(HashSet::new());
            if let Some(curr) = scopes.last_mut() {
                curr.insert(item_var.name.clone());
                if let Some(ref idx) = index_var {
                    curr.insert(idx.name.clone());
                }
            }
            for s in &body.stmts {
                validate_stmt(s, scopes, globals, doc, diags);
            }
            scopes.pop();
        }
        ExprKind::Loop { body, .. } => {
            validate_block(body, scopes, globals, doc, diags);
        }
        ExprKind::Sus {
            body,
            catches,
            finally_branch,
        } => {
            validate_block(body, scopes, globals, doc, diags);
            for c in catches {
                scopes.push(HashSet::new());
                if let Some(curr) = scopes.last_mut() {
                    curr.insert("errkind".to_string());
                    if let Some(ref err) = c.error_var {
                        curr.insert(err.name.clone());
                    }
                }
                for s in &c.body.stmts {
                    validate_stmt(s, scopes, globals, doc, diags);
                }
                scopes.pop();
            }
            if let Some(ref fin) = finally_branch {
                validate_block(fin, scopes, globals, doc, diags);
            }
        }
        ExprKind::Switch { target, arms } => {
            validate_expr(target, scopes, globals, doc, diags);
            let has_catch_all = arms.iter().any(|a| match &a.pattern.kind {
                PatternKind::Wildcard => true,
                PatternKind::Ident(_) => a.guard.is_none(),
                _ => false,
            });
            if !has_catch_all {
                let range = doc.span_to_range(expr.span.start, expr.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0405".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: "Non-exhaustive switch: missing 'else ->' fallback arm".to_string(),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
            for arm in arms {
                scopes.push(HashSet::new());
                bind_pattern(&arm.pattern, scopes);
                if let Some(ref guard) = arm.guard {
                    validate_expr(guard, scopes, globals, doc, diags);
                }
                for s in &arm.body.stmts {
                    validate_stmt(s, scopes, globals, doc, diags);
                }
                scopes.pop();
            }
        }
        ExprKind::Lambda(params, body) => {
            scopes.push(HashSet::new());
            if let Some(curr) = scopes.last_mut() {
                for p in params {
                    curr.insert(p.name.clone());
                }
            }
            validate_expr(body, scopes, globals, doc, diags);
            scopes.pop();
        }
        ExprKind::InterpolatedString(ref parts) => {
            for p in parts {
                if let StringPart::Expr(ref e) = p {
                    validate_expr(e, scopes, globals, doc, diags);
                }
            }
        }
        ExprKind::Path(ref parts) => {
            if let Some(first) = parts.first() {
                if !is_in_scope(&first.name, scopes, globals) {
                    let range = doc.span_to_range(first.span.start, first.span.end);
                    diags.push(LspDiagnostic {
                        range,
                        severity: Some(LspSeverity::ERROR),
                        code: Some(NumberOrString::String("E0305".to_string())),
                        code_description: None,
                        source: Some("yukino".to_string()),
                        message: format!("Cannot find symbol '{}' in this scope", first.name),
                        related_information: None,
                        tags: None,
                        data: None,
                    });
                }
            }
        }
        _ => {}
    }
}

fn is_in_scope(name: &str, scopes: &[HashSet<String>], globals: &HashSet<String>) -> bool {
    for s in scopes.iter().rev() {
        if s.contains(name) {
            return true;
        }
    }
    globals.contains(name)
}

fn bind_target(target: &BindingTarget, scopes: &mut [HashSet<String>]) {
    let Some(curr) = scopes.last_mut() else { return; };
    bind_target_inner(target, curr);
}

fn bind_target_inner(target: &BindingTarget, set: &mut HashSet<String>) {
    match target {
        BindingTarget::Ident(ref id) => {
            set.insert(id.name.clone());
        }
        BindingTarget::Tuple(ref items) => {
            for it in items {
                bind_target_inner(it, set);
            }
        }
        BindingTarget::Array(ref items) => {
            for it in items {
                bind_target_inner(&it.target, set);
            }
        }
        BindingTarget::Map(ref fields) => {
            for f in fields {
                if let Some(ref alias) = f.alias {
                    bind_target_inner(alias, set);
                } else {
                    set.insert(f.field.name.clone());
                }
            }
        }
    }
}

fn bind_pattern(pat: &Pattern, scopes: &mut [HashSet<String>]) {
    let Some(curr) = scopes.last_mut() else { return; };
    bind_pattern_inner(&pat.kind, curr);
}

fn bind_pattern_inner(kind: &PatternKind, set: &mut HashSet<String>) {
    match kind {
        PatternKind::Ident(ref id) => {
            set.insert(id.name.clone());
        }
        PatternKind::Tuple(ref pats) => {
            for p in pats {
                bind_pattern_inner(&p.kind, set);
            }
        }
        PatternKind::Struct(_, ref fields) => {
            for (_, p) in fields {
                bind_pattern_inner(&p.kind, set);
            }
        }
        PatternKind::Variant(_, _, ref pats) => {
            for p in pats {
                bind_pattern_inner(&p.kind, set);
            }
        }
        PatternKind::Or(ref pats) => {
            for p in pats {
                bind_pattern_inner(&p.kind, set);
            }
        }
        _ => {}
    }
}

fn validate_lvalue(
    lval: &LValue,
    scopes: &mut Vec<HashSet<String>>,
    globals: &HashSet<String>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    match lval {
        LValue::Ident(ref id) => {
            if !is_in_scope(&id.name, scopes, globals) {
                let range = doc.span_to_range(id.span.start, id.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0305".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: format!("Cannot find symbol '{}' in this scope", id.name),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
        }
        LValue::Field(ref expr, _) => {
            validate_expr(expr, scopes, globals, doc, diags);
        }
        LValue::Index(ref expr, ref idx) => {
            validate_expr(expr, scopes, globals, doc, diags);
            validate_expr(idx, scopes, globals, doc, diags);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::Url;

    #[test]
    fn test_undefined_symbol_diagnostic() {
        let text = "fun test() {\n    set x = zalupa\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.message.contains("zalupa")),
            "expected undefined symbol error for zalupa, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_numeric_bounds_overflow() {
        let text = "fun test() {\n    set x: int16 = 123121231231231231231\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.message.contains("int16")),
            "expected out of range error for int16, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_numeric_bounds_70000() {
        let text = "fun test() {\n    set x: int16 = 70000\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.message.contains("int16")),
            "expected out of range error for int16, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_catch_errkind_no_undefined() {
        let text = "fun test() {\n    sus {\n        set a = 1\n    } catch {\n        set k = errkind\n    }\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            !diags.iter().any(|d| d.message.contains("errkind")),
            "errkind should never be marked undefined, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_views_spes_classes_resolved() {
        let path = Path::new("/home/shareui/dev/yuki-lang/source/offlibs/siesta/apps/demosecond/src/views.ykn");
        let uri = Url::from_file_path(path).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        let doc = Document::new(uri, content, 1);
        let env = WorkspaceEnv::for_path(path);
        let (_, diags) = analyze_document(&doc, Some(&env));

        let undefined_classes: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("Cannot find symbol 'ShowcaseTheme'") || d.message.contains("Cannot find symbol 'OverviewScreen'"))
            .collect();
        assert!(
            undefined_classes.is_empty(),
            "ShowcaseTheme and OverviewScreen should not be undefined, got: {:?}",
            undefined_classes
        );
    }

    #[test]
    fn test_this_outside_class_is_undefined() {
        let text = "fun foo() {\n    this.zalupa()\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.message.contains("Cannot find symbol 'this' in this scope")),
            "this outside class should be flagged as undefined, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_this_inside_class_is_valid() {
        let text = "class Test {\n    public name: string\n    fun init(a: string) {\n        rewrite this.name = a\n    }\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            !diags.iter().any(|d| d.message.contains("Cannot find symbol 'this'")),
            "this inside class should be valid, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_struct_init_missing_and_unknown_fields() {
        let text = "struct Any {\n    aa: int64\n}\nfun foo() {\n    set x = Any {\n        bb: 10,\n    }\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.message.contains("has no field named 'bb'")),
            "unknown field 'bb' should be reported, got: {:?}",
            diags
        );
        assert!(
            diags.iter().any(|d| d.message.contains("Missing field 'aa' in initializer")),
            "missing field 'aa' should be reported, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_type_mismatch_and_return_checks() {
        let text = "struct Any {\n    aa: int64\n}\nfun zalupa() -> Any {\n}\nfun bar() -> int32 {\n    return \"hello\"\n}\nfun baz() {\n    return 10\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.message.contains("Missing return statement: function 'zalupa' expects return of type 'Any'")),
            "missing return for zalupa should be reported, got: {:?}",
            diags
        );
        assert!(
            diags.iter().any(|d| d.message.contains("Mismatched types: expected return type 'int32', found 'string'")),
            "mismatched return type for bar should be reported, got: {:?}",
            diags
        );
        assert!(
            diags.iter().any(|d| d.message.contains("Function 'baz' with void return type cannot return a value")),
            "void function baz returning value should be reported, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_call_arguments_checks() {
        let text = "fun add(a: int32, b: string) -> void {\n    space\n}\nfun foo() {\n    add(1)\n    add(1, 2)\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.message.contains("takes 2 argument(s) but 1 were supplied")),
            "argument count mismatch should be reported, got: {:?}",
            diags
        );
        assert!(
            diags.iter().any(|d| d.message.contains("expected 'string', found 'int64' for argument 'b'")),
            "argument type mismatch should be reported, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_manifest_allow_undef() {
        let text = "fun foo() {\n    custom_macro_fn(1, 2, 3)\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let mut env = WorkspaceEnv::for_path(Path::new("."));
        env.allow_undef.insert("custom_macro_fn".to_string());
        let (_, diags) = analyze_document(&doc, Some(&env));

        assert!(
            !diags.iter().any(|d| d.message.contains("custom_macro_fn")),
            "symbol in allow_undef should not be reported as undefined, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_symbol_conflict_e0304() {
        let text = "fun foo() {}\nfun foo() {}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0304".to_string()))),
            "duplicate symbol should trigger E0304, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_subclass_locked_class_e0409() {
        let text = "locked class Base {}\nclass Child extends Base {}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0409".to_string()))),
            "extending locked class should trigger E0409, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_invalid_super_call_e0416() {
        let text = "class Standalone {\n    fun bar() {\n        super.bar()\n    }\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0416".to_string()))),
            "super in class without superclass should trigger E0416, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_non_exhaustive_switch_e0405() {
        let text = "fun test_switch(x: int32) {\n    switch x {\n        1 -> println(\"one\")\n        2 -> println(\"two\")\n    }\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0405".to_string()))),
            "switch without else arm should trigger E0405, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_method_not_found_e0403() {
        let text = "class Greeter {\n    public fun hello() {}\n}\nfun test() {\n    set g = init Greeter()\n    g.missing_method()\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0403".to_string()))),
            "calling undefined method should trigger E0403, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_unused_variable_w0001_and_pointless_mutability_w0005() {
        let text = "fun test() {\n    set unused = 42\n    set read_only = 10\n    println(read_only)\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("W0001".to_string())) && d.message.contains("unused")),
            "unused variable should trigger W0001, got: {:?}",
            diags
        );
        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("W0005".to_string())) && d.message.contains("read_only")),
            "never rewritten variable should trigger W0005, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_unreachable_code_w0004() {
        let text = "fun test() -> int32 {\n    return 1\n    println(\"unreachable\")\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("W0004".to_string()))),
            "statement after return should trigger W0004, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_lint_allow_suppression() {
        let text = "#![lint::allow(\"unused_variable\")]\nfun test() {\n    set unused = 42\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            !diags.iter().any(|d| d.code == Some(NumberOrString::String("W0001".to_string()))),
            "W0001 should be suppressed by lint::allow, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_lint_deny_escalates_to_error() {
        let text = "#![lint::deny(\"unreachable_code\")]\nfun test() -> int32 {\n    return 1\n    println(\"unreachable\")\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        let denied = diags.iter().find(|d| d.code == Some(NumberOrString::String("W0004".to_string())));
        assert!(denied.is_some(), "expected W0004 diagnostic");
        assert_eq!(
            denied.unwrap().severity,
            Some(LspSeverity::ERROR),
            "W0004 should be escalated to ERROR under lint::deny"
        );
    }

    #[test]
    fn test_empty_array_assignment_allowed() {
        let text = "fun test() {\n    set dl: bool[] = []\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        let type_errors: Vec<_> = diags.iter().filter(|d| d.code == Some(NumberOrString::String("E0401".to_string()))).collect();
        assert!(type_errors.is_empty(), "empty array should not trigger E0401, got: {:?}", type_errors);
    }

    #[test]
    fn test_static_array_type_checked() {
        let text = "static a: int64[] = [\"sdada\"]\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0401".to_string())) && d.message.contains("int64")),
            "static a: int64[] = [\"sdada\"] should trigger E0401, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_static_variable_in_scope() {
        let text = "struct StTest {\n    a: string\n    b: int32\n}\nfun main() {\n    static x = StTest {\n        a: \"adasdasd\",\n        b: 132,\n    }\n    set val = x.a\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        let undefined_x = diags.iter().any(|d| d.code == Some(NumberOrString::String("E0305".to_string())) && d.message.contains("'x'"));
        assert!(!undefined_x, "static variable x should be in scope, got: {:?}", diags);
    }

    #[test]
    fn test_fixed_size_array_matching_size() {
        let text = "fun test() {\n    set a: int64[2] = [1, 2]\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        let type_errors: Vec<_> = diags.iter().filter(|d| d.code == Some(NumberOrString::String("E0401".to_string()))).collect();
        assert!(type_errors.is_empty(), "int64[2] = [1, 2] should not trigger E0401, got: {:?}", type_errors);
    }

    #[test]
    fn test_fixed_size_array_mismatch_and_partial() {
        let text = "fun test() {\n    set a: int64[2] = [1, 2, 3]\n    set b: int64[3] = [1]\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0401".to_string())) && d.message.contains("size mismatch")),
            "excess elements should trigger E0401 size mismatch, got: {:?}",
            diags
        );
        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("W0404".to_string())) && d.message.contains("partially initialized")),
            "fewer elements should trigger W0404 partial initialization, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_array_indexing_bounds_and_type() {
        let text = "fun test() {\n    set a: int64[2] = [1, 2]\n    set oob = a[5]\n    set bad_idx = a[\"wrong\"]\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0401".to_string())) && d.message.contains("out of bounds")),
            "a[5] on int64[2] should trigger out of bounds E0401, got: {:?}",
            diags
        );
        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0401".to_string())) && d.message.contains("expected integer index")),
            "a[\"wrong\"] should trigger integer index E0401, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_static_reassignment_forbidden_e0503() {
        let text = "fun test() {\n    static x = 10\n    rewrite x = 20\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0503".to_string()))),
            "reassigning static x should trigger E0503, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_empty_array_without_type_context_e0402() {
        let text = "fun test() {\n    set x = []\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0402".to_string()))),
            "empty array literal without context should trigger E0402, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_ref_argument_type_checking() {
        let text = r#"
fun takes_val(x: int32) {}
fun takes_ref(ref x: int32) {}
fun takes_mut_ref(ref ~x: int32) {}

fun test() {
    set a: int32 = 10
    takes_val(ref a)
    takes_ref(a)
    takes_mut_ref(ref a)

    takes_val(a)
    takes_ref(ref a)
    takes_ref(ref ~a)
    takes_mut_ref(ref ~a)
}
"#;
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        let e0401_msgs: Vec<String> = diags
            .iter()
            .filter(|d| d.code == Some(NumberOrString::String("E0401".to_string())))
            .map(|d| d.message.clone())
            .collect();

        assert!(
            e0401_msgs.iter().any(|m| m.contains("expected 'int32', found 'ref int32'")),
            "takes_val(ref a) should fail with mismatched types, got: {:?}",
            e0401_msgs
        );
        assert!(
            e0401_msgs.iter().any(|m| m.contains("expected 'ref int32', found 'int32'")),
            "takes_ref(a) should fail with mismatched types, got: {:?}",
            e0401_msgs
        );
        assert!(
            e0401_msgs.iter().any(|m| m.contains("expected 'ref ~int32', found 'ref int32'")),
            "takes_mut_ref(ref a) should fail with mismatched types, got: {:?}",
            e0401_msgs
        );
        assert_eq!(e0401_msgs.len(), 3, "Only the 3 invalid calls should produce E0401, got: {:?}", e0401_msgs);
    }

    #[test]
    fn test_static_mut_ref_forbidden_e0503() {
        let text = "static s: int32 = 42\nfun takes_mut(x: ref ~int32) {}\nfun test() {\n    takes_mut(ref ~s)\n}\n";
        let uri = Url::parse("file:///test.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        assert!(
            diags.iter().any(|d| d.code == Some(NumberOrString::String("E0503".to_string()))),
            "ref ~s on static should trigger E0503, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_class_init_and_imported_method_diagnostics() {
        let path = Path::new("/home/shareui/dev/yuki-lang/source/exm/src/main.ykn");
        let uri = Url::from_file_path(path).unwrap();
        let env = WorkspaceEnv::for_path(path);

        // Case 1: init Test() with missing argument
        let text1 = "load .utils.test: Test\nfun main() {\n    set t = init Test()\n}\n";
        let doc1 = Document::new(uri.clone(), text1.to_string(), 1);
        let (_, diags1) = analyze_document(&doc1, Some(&env));
        assert!(
            diags1.iter().any(|d| d.code == Some(NumberOrString::String("E0401".to_string())) && d.message.contains("Test.init")),
            "init Test() with 0 args should fail E0401, got: {:?}",
            diags1
        );

        // Case 2: extra argument to method and type mismatch on assignment
        let text2 = "load .utils.test: Test\nfun main() {\n    set t = init Test(-132)\n    set x: string = t.abs(\"asd\")\n}\n";
        let doc2 = Document::new(uri, text2.to_string(), 1);
        let (_, diags2) = analyze_document(&doc2, Some(&env));
        assert!(
            diags2.iter().any(|d| d.code == Some(NumberOrString::String("E0401".to_string())) && d.message.contains("abs")),
            "t.abs(\"asd\") should fail with argument count, got: {:?}",
            diags2
        );
        assert!(
            diags2.iter().any(|d| d.code == Some(NumberOrString::String("E0401".to_string())) && d.message.contains("expected 'string', found 'int64'")),
            "set x: string = t.abs() should fail with type mismatch, got: {:?}",
            diags2
        );
    }

    #[test]
    fn test_import_with_as_no_undefined_symbol() {
        let path = Path::new("/home/shareui/dev/yuki-lang/source/exm/src/main.ykn");
        let uri = Url::from_file_path(path).unwrap();
        let env = WorkspaceEnv::for_path(path);
        let text = "load std.math: abs as abs_int\nfun main() {\n    set res = abs_int(-42.0)\n}\n";
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, Some(&env));
        let undefined_abs_int = diags.iter().any(|d| d.code == Some(NumberOrString::String("E0305".to_string())));
        assert!(!undefined_abs_int, "aliased import abs_int must not trigger undefined symbol E0305, got: {:?}", diags);
    }

    #[test]
    fn test_ref_param_rewrite_and_generic_cast() {
        let text = "fun helper(ref ~step: int64) {\n    rewrite step = 3\n}\nfun calc() {\n    set a: int64 = 10\n    set s = cast(a, \"string\")\n}\n";
        let uri = Url::parse("file:///test_ref.ykn").unwrap();
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, None);

        let type_errors: Vec<_> = diags.iter().filter(|d| d.code == Some(NumberOrString::String("E0401".to_string()))).collect();
        assert!(type_errors.is_empty(), "expected no E0401 type errors, got: {:?}", type_errors);
    }

    #[test]
    fn test_workspace_dependency_load_siesta() {
        let path = Path::new("/home/shareui/dev/yuki-lang/source/exm/src/main.ykn");
        let uri = Url::from_file_path(path).unwrap();
        let env = WorkspaceEnv::for_path(path);
        let text = "load siesta\n";
        let doc = Document::new(uri, text.to_string(), 1);
        let (_, diags) = analyze_document(&doc, Some(&env));

        let module_errors: Vec<_> = diags.iter().filter(|d| d.code == Some(NumberOrString::String("E0301".to_string()))).collect();
        assert!(module_errors.is_empty(), "load siesta should find siesta library dependency, got: {:?}", module_errors);
    }
}
