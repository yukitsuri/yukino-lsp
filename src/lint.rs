use std::collections::HashSet;
use tower_lsp::lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticSeverity as LspSeverity, NumberOrString,
};
use yukic_ast::attr::{AttributeArgs, AttributeNode};
use yukic_ast::expr::{Expr, ExprKind, LiteralValue};
use yukic_ast::item::{Item, ItemKind, Visibility};
use yukic_ast::stmt::{BindingTarget, Block, LValue, Stmt, StmtKind};
use yukic_ast::Module;
use yukic_diagnostics::code::{DiagnosticCode, WarningCode};
use yukic_diagnostics::filter::WarningFilter;

use crate::document::Document;

pub fn run_lint_checks(
    module: &Module,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let filter = extract_warning_filter(module);

    check_unused_imports(module, &filter, doc, diags);
    check_dead_functions(module, &filter, doc, diags);
    check_module_blocks(module, &filter, doc, diags);
    apply_filter_to_all_diags(&filter, diags);
}

pub fn extract_warning_filter(module: &Module) -> WarningFilter {
    let mut filter = WarningFilter::new();
    extract_from_attrs(&module.module_attrs, &mut filter);

    for item in &module.items {
        extract_from_attrs(&item.attrs, &mut filter);
        match &item.kind {
            ItemKind::Fun(f) => extract_from_attrs(&f.attrs, &mut filter),
            ItemKind::Class(c) => {
                for m in &c.methods {
                    extract_from_attrs(&m.attrs, &mut filter);
                }
            }
            ItemKind::Struct(s) => {
                for m in &s.methods {
                    extract_from_attrs(&m.attrs, &mut filter);
                }
            }
            _ => {}
        }
    }
    filter
}

fn extract_from_attrs(attrs: &[AttributeNode], filter: &mut WarningFilter) {
    for attr in attrs {
        let name = attr.path_str();
        let is_allow = is_lint_allow(&name);
        let is_deny = is_lint_deny(&name);
        if !is_allow && !is_deny {
            continue;
        }
        apply_lint_attr_args(attr, is_deny, filter);
    }
}

fn is_lint_allow(name: &str) -> bool {
    name == "lint::allow" || name == "lint::ignore" || name == "ignore_warn" || name == "warns_ignore"
}

fn is_lint_deny(name: &str) -> bool {
    name == "lint::deny" || name == "deny_warn" || name == "warns_deny"
}

fn apply_lint_attr_args(attr: &AttributeNode, is_deny: bool, filter: &mut WarningFilter) {
    match &attr.args {
        AttributeArgs::Positional(exprs) => {
            for expr in exprs {
                apply_single_rule(expr, is_deny, filter);
            }
        }
        AttributeArgs::Named(pairs) => {
            for (_, expr) in pairs {
                apply_single_rule(expr, is_deny, filter);
            }
        }
        AttributeArgs::None => {}
    }
}

fn apply_single_rule(expr: &Expr, is_deny: bool, filter: &mut WarningFilter) {
    let rule_name = match &expr.kind {
        ExprKind::Literal(LiteralValue::String(s)) => Some(s.as_str()),
        ExprKind::Ident(id) => Some(id.name.as_str()),
        _ => None,
    };
    let Some(name) = rule_name else { return; };
    if is_deny {
        filter.deny_name(name);
    } else {
        filter.ignore_name(name);
    }
}

fn check_unused_imports(
    module: &Module,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let mut usages = HashSet::new();
    for item in &module.items {
        if !matches!(item.kind, ItemKind::Import(_)) {
            collect_ident_usages_item(item, &mut usages);
        }
    }

    for item in &module.items {
        if item.visibility == Visibility::Public {
            continue;
        }
        let ItemKind::Import(ref imp) = item.kind else { continue; };
        let Some(ref clause) = imp.symbols else { continue; };

        for (idx, sym_item) in clause.items.iter().enumerate() {
            let yukic_ast::ImportPattern::Exact(ref id) = sym_item.pattern else { continue; };
            let imported_name = clause
                .aliases
                .get(idx)
                .map(|a| a.name.as_str())
                .unwrap_or(id.name.as_str());

            if !usages.contains(imported_name) {
                emit_warning(
                    WarningCode::W0002,
                    format!("unused import '{imported_name}'"),
                    id.span.start,
                    id.span.end,
                    filter,
                    doc,
                    diags,
                );
            }
        }
    }
}

fn check_dead_functions(
    module: &Module,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let mut usages = HashSet::new();
    for item in &module.items {
        collect_ident_usages_item(item, &mut usages);
    }

    for item in &module.items {
        let ItemKind::Fun(ref fun) = item.kind else { continue; };
        if fun.name.name == "main" || item.visibility == Visibility::Public {
            continue;
        }
        if fun.attrs.iter().any(|a| a.is("test")) {
            if fun.attrs.iter().any(|a| a.is("ignore")) {
                emit_warning(
                    WarningCode::W0423,
                    format!("test function '{}' is marked as ignored", fun.name.name),
                    fun.name.span.start,
                    fun.name.span.end,
                    filter,
                    doc,
                    diags,
                );
            }
            continue;
        }
        if !usages.contains(&fun.name.name) {
            emit_warning(
                WarningCode::W0003,
                format!("function '{}' is never used", fun.name.name),
                fun.name.span.start,
                fun.name.span.end,
                filter,
                doc,
                diags,
            );
        }
    }
}

fn check_module_blocks(
    module: &Module,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(f) => {
                if let Some(ref body) = f.body {
                    check_block(body, filter, doc, diags);
                }
            }
            ItemKind::Cfun(cf) => {
                if let Some(ref body) = cf.body {
                    check_block(body, filter, doc, diags);
                }
            }
            ItemKind::Class(c) => {
                for m in &c.methods {
                    if let Some(ref body) = m.body {
                        check_block(body, filter, doc, diags);
                    }
                }
                for a in &c.accessors {
                    if let Some(ref body) = a.body {
                        check_block(body, filter, doc, diags);
                    }
                }
            }
            ItemKind::Struct(s) => {
                for m in &s.methods {
                    if let Some(ref body) = m.body {
                        check_block(body, filter, doc, diags);
                    }
                }
            }
            _ => {}
        }
    }
}

fn check_block(
    block: &Block,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    check_unused_vars_in_block(block, filter, doc, diags);
    check_unreachable_in_block(block, filter, doc, diags);

    for stmt in &block.stmts {
        check_stmt_nested_blocks(stmt, filter, doc, diags);
    }
}

fn check_stmt_nested_blocks(
    stmt: &Stmt,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    match &stmt.kind {
        StmtKind::Expr(expr) => check_expr_nested_blocks(expr, filter, doc, diags),
        StmtKind::Defer(blk) => check_block(blk, filter, doc, diags),
        _ => {}
    }
}

fn check_expr_nested_blocks(
    expr: &Expr,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    match &expr.kind {
        ExprKind::Block(blk)
        | ExprKind::RegionBlock(blk)
        | ExprKind::RegionScopedBlock(_, blk)
        | ExprKind::Loop { body: blk, .. } => check_block(blk, filter, doc, diags),
        ExprKind::If {
            cond,
            then_branch,
            elif_branches,
            else_branch,
            ..
        } => {
            check_condition_predictability(cond, filter, doc, diags);
            check_block(then_branch, filter, doc, diags);
            for (c, elif_blk) in elif_branches {
                check_condition_predictability(c, filter, doc, diags);
                check_block(elif_blk, filter, doc, diags);
            }
            if let Some(else_blk) = else_branch {
                check_block(else_blk, filter, doc, diags);
            }
        }
        ExprKind::While { cond, body, .. } => {
            check_condition_predictability(cond, filter, doc, diags);
            check_block(body, filter, doc, diags);
        }
        ExprKind::For { body, .. } => check_block(body, filter, doc, diags),
        ExprKind::Switch { arms, .. } => {
            for arm in arms {
                check_block(&arm.body, filter, doc, diags);
            }
        }
        ExprKind::Sus { body, catches, finally_branch } => {
            check_block(body, filter, doc, diags);
            for c in catches {
                check_block(&c.body, filter, doc, diags);
            }
            if let Some(fin) = finally_branch {
                check_block(fin, filter, doc, diags);
            }
        }
        _ => {}
    }
}

fn check_condition_predictability(
    cond: &Expr,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    if let ExprKind::Literal(LiteralValue::Bool(b)) = &cond.kind {
        emit_warning(
            WarningCode::W0412,
            format!("predictable condition '{b}', will always evaluate the same way"),
            cond.span.start,
            cond.span.end,
            filter,
            doc,
            diags,
        );
    }
}

fn check_unused_vars_in_block(
    block: &Block,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    for (i, stmt) in block.stmts.iter().enumerate() {
        let (bound, literal_kind) = match &stmt.kind {
            StmtKind::Set { target, init, .. } => {
                let mut ids = Vec::new();
                collect_bound_idents(target, &mut ids);
                let lit_info = match target {
                    BindingTarget::Ident(id) => {
                        classify_literal_init(init).map(|kind| (id.clone(), kind))
                    }
                    _ => None,
                };
                (ids, lit_info)
            }
            StmtKind::Static(s) => (vec![s.name.clone()], None),
            _ => (Vec::new(), None),
        };

        if bound.is_empty() {
            continue;
        }

        let mut subsequent_usages = HashSet::new();
        let mut mutated = HashSet::new();
        let mut method_called = HashSet::new();
        for subsequent in &block.stmts[i + 1..] {
            collect_ident_usages_stmt(subsequent, &mut subsequent_usages);
            collect_mutated_idents_stmt(subsequent, &mut mutated, &mut method_called);
        }

        for id in bound {
            if id.name.starts_with('_') {
                continue;
            }
            if !subsequent_usages.contains(&id.name) {
                emit_warning(
                    WarningCode::W0001,
                    format!("unused variable '{}'", id.name),
                    id.span.start,
                    id.span.end,
                    filter,
                    doc,
                    diags,
                );
            } else if let Some((ref set_id, kind)) = literal_kind {
                if set_id.name == id.name
                    && !mutated.contains(&id.name)
                    && (kind == LiteralInitKind::Scalar || !method_called.contains(&id.name))
                {
                    emit_warning(
                        WarningCode::W0005,
                        format!(
                            "variable '{}' is initialized with a constant literal and never mutated; consider using 'static' instead of 'set'",
                            id.name
                        ),
                        id.span.start,
                        id.span.end,
                        filter,
                        doc,
                        diags,
                    );
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LiteralInitKind {
    Scalar,
    Aggregate,
}

fn classify_literal_init(expr: &Expr) -> Option<LiteralInitKind> {
    match &expr.kind {
        ExprKind::Literal(_) => Some(LiteralInitKind::Scalar),
        ExprKind::Unary(_, inner) => classify_literal_init(inner),
        ExprKind::Binary(op, l, r) => {
            use yukic_ast::expr::BinaryOp;
            if matches!(op, BinaryOp::Range | BinaryOp::RangeInclusive) {
                return None;
            }
            let lk = classify_literal_init(l)?;
            let rk = classify_literal_init(r)?;
            if lk == LiteralInitKind::Scalar && rk == LiteralInitKind::Scalar {
                Some(LiteralInitKind::Scalar)
            } else {
                None
            }
        }
        ExprKind::Ternary(c, t, e) => {
            if classify_literal_init(c)? == LiteralInitKind::Scalar {
                let tk = classify_literal_init(t)?;
                let ek = classify_literal_init(e)?;
                if tk == ek {
                    Some(tk)
                } else {
                    Some(LiteralInitKind::Aggregate)
                }
            } else {
                None
            }
        }
        ExprKind::ArrayInit(items) | ExprKind::TupleInit(items) if !items.is_empty() => {
            for item in items {
                classify_literal_init(item)?;
            }
            Some(LiteralInitKind::Aggregate)
        }
        _ => None,
    }
}

fn root_ident_of_expr(mut expr: &Expr) -> Option<&str> {
    loop {
        match &expr.kind {
            ExprKind::Ident(id) => return Some(id.name.as_str()),
            ExprKind::FieldAccess(base, _)
            | ExprKind::IndexAccess(base, _)
            | ExprKind::SliceAccess(base, _, _, _) => expr = base,
            _ => return None,
        }
    }
}

fn check_unreachable_in_block(
    block: &Block,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let mut terminated = false;
    for stmt in &block.stmts {
        if terminated {
            emit_warning(
                WarningCode::W0004,
                "unreachable code after terminal statement".to_string(),
                stmt.span.start,
                stmt.span.end,
                filter,
                doc,
                diags,
            );
            break;
        }
        if is_terminating_stmt(stmt) {
            terminated = true;
        }
    }
}

fn is_terminating_stmt(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Return(_) | StmtKind::Recur(_) | StmtKind::Break(_) | StmtKind::Continue(_) => true,
        StmtKind::Expr(e) => {
            if let ExprKind::Call(ref callee, _) = e.kind {
                if let ExprKind::Ident(ref id) = callee.kind {
                    return id.name == "panic" || id.name == "exit";
                }
            }
            false
        }
        _ => false,
    }
}

fn collect_bound_idents(target: &BindingTarget, out: &mut Vec<yukic_ast::Ident>) {
    match target {
        BindingTarget::Ident(id) => out.push(id.clone()),
        BindingTarget::Tuple(targets) => {
            for t in targets {
                collect_bound_idents(t, out);
            }
        }
        BindingTarget::Array(elements) => {
            for elem in elements {
                collect_bound_idents(&elem.target, out);
            }
        }
        BindingTarget::Map(fields) => {
            for f in fields {
                if let Some(alias) = &f.alias {
                    collect_bound_idents(alias, out);
                } else {
                    out.push(f.field.clone());
                }
            }
        }
    }
}

fn collect_ident_usages_item(item: &Item, usages: &mut HashSet<String>) {
    match &item.kind {
        ItemKind::Fun(f) => {
            usages.insert(f.name.name.clone());
            if let Some(ref body) = f.body {
                collect_ident_usages_block(body, usages);
            }
        }
        ItemKind::Class(c) => {
            usages.insert(c.name.name.clone());
            for m in &c.methods {
                usages.insert(m.name.name.clone());
                if let Some(ref body) = m.body {
                    collect_ident_usages_block(body, usages);
                }
            }
            for a in &c.accessors {
                if let Some(ref body) = a.body {
                    collect_ident_usages_block(body, usages);
                }
            }
        }
        ItemKind::Struct(s) => {
            usages.insert(s.name.name.clone());
            for m in &s.methods {
                usages.insert(m.name.name.clone());
                if let Some(ref body) = m.body {
                    collect_ident_usages_block(body, usages);
                }
            }
        }
        ItemKind::Static(s) => {
            usages.insert(s.name.name.clone());
            collect_ident_usages_expr(&s.init, usages);
        }
        _ => {}
    }
}

fn collect_ident_usages_block(block: &Block, usages: &mut HashSet<String>) {
    for stmt in &block.stmts {
        collect_ident_usages_stmt(stmt, usages);
    }
}

fn collect_ident_usages_stmt(stmt: &Stmt, usages: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => {
            collect_ident_usages_expr(e, usages);
        }
        StmtKind::Recur(args) => {
            for a in args {
                collect_ident_usages_expr(a, usages);
            }
        }
        StmtKind::Set { init, .. } => {
            collect_ident_usages_expr(init, usages);
        }
        StmtKind::Rewrite { target, value, .. } => {
            collect_ident_usages_lvalue(target, usages);
            collect_ident_usages_expr(value, usages);
        }
        StmtKind::Static(s) => {
            collect_ident_usages_expr(&s.init, usages);
        }
        StmtKind::Defer(blk) => {
            collect_ident_usages_block(blk, usages);
        }
        _ => {}
    }
}

fn collect_ident_usages_lvalue(lval: &LValue, usages: &mut HashSet<String>) {
    match lval {
        LValue::Ident(id) => {
            usages.insert(id.name.clone());
        }
        LValue::Field(obj, _) => {
            collect_ident_usages_expr(obj, usages);
        }
        LValue::Index(obj, idx) => {
            collect_ident_usages_expr(obj, usages);
            collect_ident_usages_expr(idx, usages);
        }
    }
}

fn collect_mutated_idents_stmt(
    stmt: &Stmt,
    mutated: &mut HashSet<String>,
    method_called: &mut HashSet<String>,
) {
    match &stmt.kind {
        StmtKind::Rewrite { target, value, .. } => {
            match target {
                LValue::Ident(id) => {
                    mutated.insert(id.name.clone());
                }
                LValue::Field(base, _) => {
                    if let Some(root) = root_ident_of_expr(base) {
                        mutated.insert(root.to_string());
                    }
                    collect_mutated_idents_expr(base, mutated, method_called);
                }
                LValue::Index(base, idx) => {
                    if let Some(root) = root_ident_of_expr(base) {
                        mutated.insert(root.to_string());
                    }
                    collect_mutated_idents_expr(base, mutated, method_called);
                    collect_mutated_idents_expr(idx, mutated, method_called);
                }
            }
            collect_mutated_idents_expr(value, mutated, method_called);
        }
        StmtKind::Set { init, .. } => {
            collect_mutated_idents_expr(init, mutated, method_called);
        }
        StmtKind::Static(decl) => {
            collect_mutated_idents_expr(&decl.init, mutated, method_called);
        }
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => {
            collect_mutated_idents_expr(e, mutated, method_called);
        }
        StmtKind::Recur(args) => {
            for a in args {
                collect_mutated_idents_expr(a, mutated, method_called);
            }
        }
        StmtKind::Defer(blk) => collect_mutated_idents_block(blk, mutated, method_called),
        _ => {}
    }
}

fn collect_mutated_idents_block(
    block: &Block,
    mutated: &mut HashSet<String>,
    method_called: &mut HashSet<String>,
) {
    for stmt in &block.stmts {
        collect_mutated_idents_stmt(stmt, mutated, method_called);
    }
}

fn collect_mutated_idents_expr(
    expr: &Expr,
    mutated: &mut HashSet<String>,
    method_called: &mut HashSet<String>,
) {
    match &expr.kind {
        ExprKind::Ref { mutable: true, expr: inner } => {
            if let Some(root) = root_ident_of_expr(inner) {
                mutated.insert(root.to_string());
            }
            collect_mutated_idents_expr(inner, mutated, method_called);
        }
        ExprKind::Ref { mutable: false, expr: inner }
        | ExprKind::Unary(_, inner)
        | ExprKind::Spread(inner)
        | ExprKind::NamedArg(_, inner)
        | ExprKind::Is(inner, _) => {
            collect_mutated_idents_expr(inner, mutated, method_called);
        }
        ExprKind::PtrOf(id) => {
            mutated.insert(id.name.clone());
        }
        ExprKind::Call(callee, args) | ExprKind::ClassInit(callee, args) => {
            collect_mutated_idents_expr(callee, mutated, method_called);
            for arg in args {
                collect_mutated_idents_expr(arg, mutated, method_called);
            }
        }
        ExprKind::MethodCall(obj, _, args)
        | ExprKind::QualifiedMethodCall(obj, _, _, args)
        | ExprKind::SafeCall(obj, _, args) => {
            if let Some(root) = root_ident_of_expr(obj) {
                method_called.insert(root.to_string());
            }
            collect_mutated_idents_expr(obj, mutated, method_called);
            for arg in args {
                collect_mutated_idents_expr(arg, mutated, method_called);
            }
        }
        ExprKind::StructInit(_, fields) => {
            for f in fields {
                collect_mutated_idents_expr(&f.value, mutated, method_called);
            }
        }
        ExprKind::Binary(_, left, right) | ExprKind::Pipe(left, right) => {
            collect_mutated_idents_expr(left, mutated, method_called);
            collect_mutated_idents_expr(right, mutated, method_called);
        }
        ExprKind::Ternary(c, t, e) => {
            collect_mutated_idents_expr(c, mutated, method_called);
            collect_mutated_idents_expr(t, mutated, method_called);
            collect_mutated_idents_expr(e, mutated, method_called);
        }
        ExprKind::IndexAccess(arr, idx) => {
            collect_mutated_idents_expr(arr, mutated, method_called);
            collect_mutated_idents_expr(idx, mutated, method_called);
        }
        ExprKind::SliceAccess(arr, s, e, _) => {
            collect_mutated_idents_expr(arr, mutated, method_called);
            if let Some(s) = s {
                collect_mutated_idents_expr(s, mutated, method_called);
            }
            if let Some(e) = e {
                collect_mutated_idents_expr(e, mutated, method_called);
            }
        }
        ExprKind::FieldAccess(obj, _) => {
            collect_mutated_idents_expr(obj, mutated, method_called);
        }
        ExprKind::ArrayInit(elements) | ExprKind::TupleInit(elements) => {
            for elem in elements {
                collect_mutated_idents_expr(elem, mutated, method_called);
            }
        }
        ExprKind::MapInit(entries) => {
            for (k, v) in entries {
                collect_mutated_idents_expr(k, mutated, method_called);
                collect_mutated_idents_expr(v, mutated, method_called);
            }
        }
        ExprKind::Block(blk)
        | ExprKind::RegionBlock(blk)
        | ExprKind::RegionScopedBlock(_, blk)
        | ExprKind::Loop { body: blk, .. } => {
            collect_mutated_idents_block(blk, mutated, method_called);
        }
        ExprKind::If { cond, then_branch, elif_branches, else_branch, .. } => {
            collect_mutated_idents_expr(cond, mutated, method_called);
            collect_mutated_idents_block(then_branch, mutated, method_called);
            for (c, b) in elif_branches {
                collect_mutated_idents_expr(c, mutated, method_called);
                collect_mutated_idents_block(b, mutated, method_called);
            }
            if let Some(b) = else_branch {
                collect_mutated_idents_block(b, mutated, method_called);
            }
        }
        ExprKind::While { cond, body, .. } => {
            collect_mutated_idents_expr(cond, mutated, method_called);
            collect_mutated_idents_block(body, mutated, method_called);
        }
        ExprKind::For { iter, body, .. } => {
            collect_mutated_idents_expr(iter, mutated, method_called);
            collect_mutated_idents_block(body, mutated, method_called);
        }
        ExprKind::Switch { target, arms, .. } => {
            collect_mutated_idents_expr(target, mutated, method_called);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_mutated_idents_expr(g, mutated, method_called);
                }
                collect_mutated_idents_block(&arm.body, mutated, method_called);
            }
        }
        ExprKind::Sus { body, catches, finally_branch } => {
            collect_mutated_idents_block(body, mutated, method_called);
            for c in catches {
                collect_mutated_idents_block(&c.body, mutated, method_called);
            }
            if let Some(fin) = finally_branch {
                collect_mutated_idents_block(fin, mutated, method_called);
            }
        }
        ExprKind::Lambda(_, body) => {
            collect_mutated_idents_expr(body, mutated, method_called);
        }
        ExprKind::AnonymousFun(fun) => {
            if let Some(ref body) = fun.body {
                collect_mutated_idents_block(body, mutated, method_called);
            }
        }
        ExprKind::InterpolatedString(parts) => {
            for part in parts {
                if let yukic_ast::expr::StringPart::Expr(e) = part {
                    collect_mutated_idents_expr(e, mutated, method_called);
                }
            }
        }
        _ => {}
    }
}

fn collect_ident_usages_expr(expr: &Expr, usages: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Ident(id) | ExprKind::PtrOf(id) => {
            usages.insert(id.name.clone());
        }
        ExprKind::Path(ids) => {
            for id in ids {
                usages.insert(id.name.clone());
            }
        }
        ExprKind::FieldAccess(obj, _) => {
            collect_ident_usages_expr(obj, usages);
        }
        ExprKind::IndexAccess(arr, idx) => {
            collect_ident_usages_expr(arr, usages);
            collect_ident_usages_expr(idx, usages);
        }
        ExprKind::SliceAccess(arr, s, e, _) => {
            collect_ident_usages_expr(arr, usages);
            if let Some(s) = s {
                collect_ident_usages_expr(s, usages);
            }
            if let Some(e) = e {
                collect_ident_usages_expr(e, usages);
            }
        }
        ExprKind::MethodCall(obj, _, args)
        | ExprKind::QualifiedMethodCall(obj, _, _, args)
        | ExprKind::SafeCall(obj, _, args) => {
            collect_ident_usages_expr(obj, usages);
            for a in args {
                collect_ident_usages_expr(a, usages);
            }
        }
        ExprKind::Call(callee, args) | ExprKind::ClassInit(callee, args) => {
            collect_ident_usages_expr(callee, usages);
            for a in args {
                collect_ident_usages_expr(a, usages);
            }
        }
        ExprKind::StructInit(name, fields) => {
            usages.insert(name.name.clone());
            for f in fields {
                collect_ident_usages_expr(&f.value, usages);
            }
        }
        ExprKind::Binary(_, left, right) | ExprKind::Pipe(left, right) => {
            collect_ident_usages_expr(left, usages);
            collect_ident_usages_expr(right, usages);
        }
        ExprKind::Ternary(c, t, e) => {
            collect_ident_usages_expr(c, usages);
            collect_ident_usages_expr(t, usages);
            collect_ident_usages_expr(e, usages);
        }
        ExprKind::Unary(_, op) | ExprKind::Spread(op) | ExprKind::NamedArg(_, op) => {
            collect_ident_usages_expr(op, usages);
        }
        ExprKind::Is(inner, _) | ExprKind::Ref { expr: inner, .. } => {
            collect_ident_usages_expr(inner, usages);
        }
        ExprKind::Block(blk) | ExprKind::RegionBlock(blk) | ExprKind::Loop { body: blk, .. } => {
            collect_ident_usages_block(blk, usages);
        }
        ExprKind::RegionScopedBlock(arg, blk) => {
            collect_ident_usages_expr(arg, usages);
            collect_ident_usages_block(blk, usages);
        }
        ExprKind::If { cond, then_branch, elif_branches, else_branch, .. } => {
            collect_ident_usages_expr(cond, usages);
            collect_ident_usages_block(then_branch, usages);
            for (c, b) in elif_branches {
                collect_ident_usages_expr(c, usages);
                collect_ident_usages_block(b, usages);
            }
            if let Some(b) = else_branch {
                collect_ident_usages_block(b, usages);
            }
        }
        ExprKind::While { cond, body, .. } => {
            collect_ident_usages_expr(cond, usages);
            collect_ident_usages_block(body, usages);
        }
        ExprKind::For { iter, body, .. } => {
            collect_ident_usages_expr(iter, usages);
            collect_ident_usages_block(body, usages);
        }
        ExprKind::Switch { target, arms, .. } => {
            collect_ident_usages_expr(target, usages);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_ident_usages_expr(g, usages);
                }
                collect_ident_usages_block(&arm.body, usages);
            }
        }
        ExprKind::ArrayInit(elements) | ExprKind::TupleInit(elements) => {
            for elem in elements {
                collect_ident_usages_expr(elem, usages);
            }
        }
        ExprKind::MapInit(entries) => {
            for (k, v) in entries {
                collect_ident_usages_expr(k, usages);
                collect_ident_usages_expr(v, usages);
            }
        }
        ExprKind::Sus { body, catches, finally_branch } => {
            collect_ident_usages_block(body, usages);
            for c in catches {
                if let Some(ref ty) = c.error_type {
                    usages.insert(ty.to_string());
                }
                collect_ident_usages_block(&c.body, usages);
            }
            if let Some(fin) = finally_branch {
                collect_ident_usages_block(fin, usages);
            }
        }
        ExprKind::Lambda(_, body) => {
            collect_ident_usages_expr(body, usages);
        }
        ExprKind::AnonymousFun(fun) => {
            usages.insert(fun.name.name.clone());
            if let Some(ref body) = fun.body {
                collect_ident_usages_block(body, usages);
            }
        }
        ExprKind::InterpolatedString(parts) => {
            for part in parts {
                if let yukic_ast::expr::StringPart::Expr(sub) = part {
                    collect_ident_usages_expr(sub, usages);
                }
            }
        }
        _ => {}
    }
}

fn emit_warning(
    code: WarningCode,
    message: String,
    start: yukic_span::BytePos,
    end: yukic_span::BytePos,
    filter: &WarningFilter,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let diag_code = DiagnosticCode::Warning(code);
    if filter.is_suppressed(&diag_code) {
        return;
    }

    let severity = if filter.is_denied(&diag_code) {
        LspSeverity::ERROR
    } else {
        LspSeverity::WARNING
    };

    let range = doc.span_to_range(start, end);
    diags.push(LspDiagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(code.as_str().to_string())),
        code_description: None,
        source: Some("yukino".to_string()),
        message,
        related_information: None,
        tags: None,
        data: None,
    });
}

fn apply_filter_to_all_diags(filter: &WarningFilter, diags: &mut Vec<LspDiagnostic>) {
    diags.retain_mut(|d| {
        let Some(NumberOrString::String(ref code_str)) = d.code else {
            return true;
        };
        if !code_str.starts_with('W') {
            return true;
        }

        let diag_code: DiagnosticCode = match code_str.parse() {
            Ok(c) => c,
            Err(_) => return true,
        };

        if filter.is_suppressed(&diag_code) {
            return false;
        }
        if filter.is_denied(&diag_code) {
            d.severity = Some(LspSeverity::ERROR);
        }
        true
    });
}
