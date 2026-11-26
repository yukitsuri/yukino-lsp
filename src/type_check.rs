#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::Path;
use tower_lsp::lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticSeverity as LspSeverity, NumberOrString,
};
use yukic_ast::*;
use crate::document::Document;
use crate::workspace::WorkspaceEnv;

#[derive(Clone, Debug)]
pub struct ParamSig {
    pub name: String,
    pub ty: Option<String>,
    pub has_default: bool,
    pub is_variadic: bool,
}

#[derive(Clone, Debug)]
pub struct FnSigInfo {
    pub name: String,
    pub params: Vec<ParamSig>,
    pub return_type: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StructFieldInfo {
    pub name: String,
    pub ty: String,
    pub has_default: bool,
}

#[derive(Clone, Debug)]
pub struct StructInfo {
    pub name: String,
    pub fields: Vec<StructFieldInfo>,
    pub methods: HashMap<String, FnSigInfo>,
    pub is_union: bool,
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub name: String,
    pub base_class: Option<String>,
    pub fields: HashMap<String, String>,
    pub static_fields: HashSet<String>,
    pub methods: HashMap<String, FnSigInfo>,
}

#[derive(Clone, Debug)]
pub struct VarInfo {
    pub ty: String,
    pub is_static: bool,
    pub fixed_size: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayTypeInfo {
    pub elem_ty: String,
    pub size: Option<usize>,
}

pub fn parse_array_type(ty: &str) -> Option<ArrayTypeInfo> {
    let s = ty.trim();
    if s == "bytes" {
        return Some(ArrayTypeInfo {
            elem_ty: "uint8".to_string(),
            size: None,
        });
    }
    if !s.ends_with(']') {
        return None;
    }
    let open_idx = s.rfind('[')?;
    let elem_ty = s[..open_idx].trim();
    if elem_ty.is_empty() {
        return None;
    }
    let size_part = s[open_idx + 1..s.len() - 1].trim();
    let size = if size_part.is_empty() {
        None
    } else {
        match size_part.parse::<usize>() {
            Ok(sz) => Some(sz),
            Err(_) => return None,
        }
    };
    Some(ArrayTypeInfo {
        elem_ty: elem_ty.to_string(),
        size,
    })
}

pub fn validate_types_and_calls(
    module: &Module,
    doc: &Document,
    current_file: &Path,
    env: Option<&WorkspaceEnv>,
    diags: &mut Vec<LspDiagnostic>,
) {
    let (fn_sigs, struct_defs, class_defs, global_vars) = collect_signatures(module, current_file, env);

    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) => {
                if let Some(ref body) = f.body {
                    let ret_ty = f.return_type.as_ref().map(|t| t.to_string());
                    check_fun_like(
                        &f.name.name,
                        &f.params,
                        ret_ty.as_deref(),
                        body,
                        None,
                        &fn_sigs,
                        &struct_defs,
                        &class_defs,
                        &global_vars,
                        doc,
                        diags,
                    );
                }
            }
            ItemKind::Class(ref c) => {
                let class_name = &c.name.name;
                let mut empty_locals = HashMap::new();
                for f in &c.fields {
                    let expected_ty = f.ty.to_string();
                    if f.is_static && f.default_val.is_none() {
                        let range = doc.span_to_range(f.span.start, f.span.end);
                        diags.push(LspDiagnostic {
                            range,
                            severity: Some(LspSeverity::ERROR),
                            code: Some(NumberOrString::String("E0401".to_string())),
                            code_description: None,
                            source: Some("yukino".to_string()),
                            message: format!(
                                "static field '{}' in class '{}' must have an initializer",
                                f.name.name, class_name
                            ),
                            related_information: None,
                            tags: None,
                            data: None,
                        });
                        continue;
                    }
                    if let Some(ref val) = f.default_val {
                        if !is_literal_expr(val) {
                            let range = doc.span_to_range(val.span.start, val.span.end);
                            diags.push(LspDiagnostic {
                                range,
                                severity: Some(LspSeverity::ERROR),
                                code: Some(NumberOrString::String("E0401".to_string())),
                                code_description: None,
                                source: Some("yukino".to_string()),
                                message: format!(
                                    "{} '{}' in class '{}' must be initialized with a literal",
                                    if f.is_static { "static field" } else { "field default" },
                                    f.name.name,
                                    class_name
                                ),
                                related_information: None,
                                tags: None,
                                data: None,
                            });
                        } else if let Some(actual) = infer_expr_type(
                            val,
                            &empty_locals,
                            &global_vars,
                            &fn_sigs,
                            &struct_defs,
                            &class_defs,
                            Some(class_name),
                        ) {
                            if !is_type_compatible(&expected_ty, &actual) {
                                let range = doc.span_to_range(val.span.start, val.span.end);
                                diags.push(LspDiagnostic {
                                    range,
                                    severity: Some(LspSeverity::ERROR),
                                    code: Some(NumberOrString::String("E0401".to_string())),
                                    code_description: None,
                                    source: Some("yukino".to_string()),
                                    message: format!(
                                        "Mismatched types: expected '{expected_ty}', found '{actual}'"
                                    ),
                                    related_information: None,
                                    tags: None,
                                    data: None,
                                });
                            }
                        }
                        check_expr(
                            val,
                            &mut empty_locals,
                            &global_vars,
                            &fn_sigs,
                            &struct_defs,
                            &class_defs,
                            Some(class_name),
                            doc,
                            diags,
                        );
                    }
                }
                for m in &c.methods {
                    if let Some(ref body) = m.body {
                        let ret_ty = m.return_type.as_ref().map(|t| t.to_string());
                        check_fun_like(
                            &m.name.name,
                            &m.params,
                            ret_ty.as_deref(),
                            body,
                            Some(class_name),
                            &fn_sigs,
                            &struct_defs,
                            &class_defs,
                            &global_vars,
                            doc,
                            diags,
                        );
                    }
                }
                for a in &c.accessors {
                    if let Some(ref body) = a.body {
                        let acc_ty = a.ty.to_string();
                        check_fun_like(
                            &a.name.name,
                            &[],
                            Some(&acc_ty),
                            body,
                            Some(class_name),
                            &fn_sigs,
                            &struct_defs,
                            &class_defs,
                            &global_vars,
                            doc,
                            diags,
                        );
                    }
                }
            }
            ItemKind::Struct(ref s) => {
                let struct_name = &s.name.name;
                for m in &s.methods {
                    if let Some(ref body) = m.body {
                        let ret_ty = m.return_type.as_ref().map(|t| t.to_string());
                        check_fun_like(
                            &m.name.name,
                            &m.params,
                            ret_ty.as_deref(),
                            body,
                            Some(struct_name),
                            &fn_sigs,
                            &struct_defs,
                            &class_defs,
                            &global_vars,
                            doc,
                            diags,
                        );
                    }
                }
            }
            ItemKind::Static(ref s) => {
                let mut local_vars = HashMap::new();
                check_var_stmt(
                    Some(&s.name),
                    None,
                    s.ty.as_ref(),
                    &s.init,
                    true,
                    &mut local_vars,
                    &global_vars,
                    &fn_sigs,
                    &struct_defs,
                    &class_defs,
                    None,
                    doc,
                    diags,
                );
            }
            _ => {}
        }
    }
}

fn collect_signatures(
    module: &Module,
    current_file: &Path,
    env: Option<&WorkspaceEnv>,
) -> (
    HashMap<String, FnSigInfo>,
    HashMap<String, StructInfo>,
    HashMap<String, ClassInfo>,
    HashMap<String, VarInfo>,
) {
    let mut fn_sigs: HashMap<String, FnSigInfo> = HashMap::new();
    let mut struct_defs: HashMap<String, StructInfo> = HashMap::new();
    let mut class_defs: HashMap<String, ClassInfo> = HashMap::new();
    let mut global_vars: HashMap<String, VarInfo> = HashMap::new();

    register_builtins(&mut fn_sigs);

    for item in &module.items {
        register_item_signatures(
            item,
            None,
            &mut fn_sigs,
            &mut struct_defs,
            &mut class_defs,
            &mut global_vars,
        );
    }

    if let Some(workspace_env) = env {
        for sym in &workspace_env.allow_undef {
            fn_sigs.entry(sym.clone()).or_insert_with(|| FnSigInfo {
                name: sym.clone(),
                params: vec![ParamSig {
                    name: "arg".to_string(),
                    ty: None,
                    has_default: true,
                    is_variadic: true,
                }],
                return_type: None,
            });
        }

        for (_decl, hdr) in workspace_env.collect_cload_headers(module, current_file) {
            if hdr.error.is_none() {
                let mut ns_aliases = HashMap::new();
                for hdr_item in &hdr.items {
                    register_item_signatures(
                        hdr_item,
                        None,
                        &mut fn_sigs,
                        &mut struct_defs,
                        &mut class_defs,
                        &mut global_vars,
                    );
                    register_ns_aliases_for_item(
                        hdr_item,
                        &hdr.namespace,
                        &mut ns_aliases,
                        &mut fn_sigs,
                        &mut struct_defs,
                        &mut global_vars,
                    );
                }
                for (k, v) in ns_aliases {
                    global_vars.entry(k).or_insert(v);
                }
            }
        }

        for item in &module.items {
            if let ItemKind::Import(ref imp) = item.kind {
                if let Some(target_file) = workspace_env.resolve_import(&imp.path, current_file) {
                    if let Some(cached) = workspace_env.get_or_parse_module(&target_file) {
                        import_cached_module_items(
                            &cached.module,
                            imp.symbols.as_ref(),
                            &mut fn_sigs,
                            &mut struct_defs,
                            &mut class_defs,
                            &mut global_vars,
                        );
                    }
                }
            }
        }
    }

    // Propagate StructInfo across typedef aliases (e.g. `typedef struct Vec3 Vec3Alias`).
    for item in &module.items {
        if let ItemKind::TypeAlias(ref ta) = item.kind {
            if let Some(s) = struct_defs.get(&ta.target.to_string()).cloned() {
                struct_defs.entry(ta.name.name.clone()).or_insert(s);
            }
        }
    }

    if let Some(parent) = current_file.parent() {
        for cls in crate::workspace::scan_spes_classes(parent) {
            class_defs.entry(cls.clone()).or_insert_with(|| ClassInfo {
                name: cls,
                base_class: None,
                fields: HashMap::new(),
                static_fields: HashSet::new(),
                methods: HashMap::new(),
            });
        }
    }

    (fn_sigs, struct_defs, class_defs, global_vars)
}

fn register_builtins(fn_sigs: &mut HashMap<String, FnSigInfo>) {
    fn_sigs.insert("println".to_string(), FnSigInfo {
        name: "println".to_string(),
        params: vec![ParamSig {
            name: "val".to_string(),
            ty: None,
            has_default: false,
            is_variadic: false,
        }],
        return_type: None,
    });
    fn_sigs.insert("print".to_string(), FnSigInfo {
        name: "print".to_string(),
        params: vec![ParamSig {
            name: "val".to_string(),
            ty: None,
            has_default: false,
            is_variadic: false,
        }],
        return_type: None,
    });
}

fn import_cached_module_items(
    target_mod: &Module,
    symbols_clause: Option<&yukic_ast::ImportSymbolsClause>,
    fn_sigs: &mut HashMap<String, FnSigInfo>,
    struct_defs: &mut HashMap<String, StructInfo>,
    class_defs: &mut HashMap<String, ClassInfo>,
    global_vars: &mut HashMap<String, VarInfo>,
) {
    let Some(clause) = symbols_clause else {
        for item in &target_mod.items {
            if item.visibility == Visibility::Public {
                register_item_signatures(item, None, fn_sigs, struct_defs, class_defs, global_vars);
            }
        }
        return;
    };

    for (idx, sym) in clause.items.iter().enumerate() {
        let alias = clause.aliases.get(idx).map(|a| a.name.as_str());
        match &sym.pattern {
            ImportPattern::All => {
                for item in &target_mod.items {
                    if item.visibility == Visibility::Public {
                        register_item_signatures(item, None, fn_sigs, struct_defs, class_defs, global_vars);
                    }
                }
            }
            ImportPattern::Exact(id) => {
                for item in &target_mod.items {
                    if get_item_name(item) == Some(id.name.as_str()) {
                        register_item_signatures(item, alias, fn_sigs, struct_defs, class_defs, global_vars);
                        break;
                    }
                }
            }
            ImportPattern::Prefix(pfx) => {
                for item in &target_mod.items {
                    if let Some(name) = get_item_name(item) {
                        if name.starts_with(pfx.as_str()) {
                            register_item_signatures(item, None, fn_sigs, struct_defs, class_defs, global_vars);
                        }
                    }
                }
            }
            ImportPattern::Suffix(sfx) => {
                for item in &target_mod.items {
                    if let Some(name) = get_item_name(item) {
                        if name.ends_with(sfx.as_str()) {
                            register_item_signatures(item, None, fn_sigs, struct_defs, class_defs, global_vars);
                        }
                    }
                }
            }
        }
    }
}

fn register_item_signatures(
    item: &Item,
    alias: Option<&str>,
    fn_sigs: &mut HashMap<String, FnSigInfo>,
    struct_defs: &mut HashMap<String, StructInfo>,
    class_defs: &mut HashMap<String, ClassInfo>,
    global_vars: &mut HashMap<String, VarInfo>,
) {
    match &item.kind {
        ItemKind::Fun(f) => {
            let name = alias.unwrap_or(&f.name.name);
            fn_sigs.insert(name.to_string(), make_fn_sig(name, &f.params, f.return_type.as_ref()));
            if alias.is_some() {
                fn_sigs.insert(f.name.name.clone(), make_fn_sig(&f.name.name, &f.params, f.return_type.as_ref()));
            }
        }
        ItemKind::Cfun(cf) => {
            let name = alias.unwrap_or(&cf.name.name);
            let sig = make_cfun_sig(name, cf);
            fn_sigs.insert(name.to_string(), sig);
        }
        ItemKind::Cstruct(cs) => {
            let name = alias.unwrap_or(&cs.name.name);
            let fields = cs.fields.iter().map(|f| StructFieldInfo {
                name: f.name.name.clone(),
                ty: f.ty.to_string(),
                has_default: f.default_val.is_some(),
            }).collect();
            struct_defs.insert(name.to_string(), StructInfo {
                name: name.to_string(),
                fields,
                methods: HashMap::new(),
                is_union: cs.is_union,
            });
        }
        ItemKind::Cvar(cv) => {
            let name = alias.unwrap_or(&cv.name.name);
            let ty = cv.ty.to_string();
            let fixed_size = parse_array_type(&ty).and_then(|a| a.size);
            global_vars.insert(name.to_string(), VarInfo {
                ty,
                is_static: false,
                fixed_size,
            });
        }
        ItemKind::Externc(ext) => {
            for ei in &ext.items {
                match ei {
                    ExterncItem::Cfun(cf) => {
                        let sig = make_cfun_sig(&cf.name.name, cf);
                        fn_sigs.insert(cf.name.name.clone(), sig.clone());
                        if let Some(ref ns) = ext.namespace {
                            fn_sigs.insert(format!("{ns}::{}", cf.name.name), sig);
                        }
                    }
                    ExterncItem::Cstruct(cs) => {
                        let fields: Vec<StructFieldInfo> = cs.fields.iter().map(|f| StructFieldInfo {
                            name: f.name.name.clone(),
                            ty: f.ty.to_string(),
                            has_default: f.default_val.is_some(),
                        }).collect();
                        let s_info = StructInfo {
                            name: cs.name.name.clone(),
                            fields,
                            methods: HashMap::new(),
                            is_union: cs.is_union,
                        };
                        struct_defs.insert(cs.name.name.clone(), s_info.clone());
                        if let Some(ref ns) = ext.namespace {
                            struct_defs.insert(format!("{ns}::{}", cs.name.name), s_info);
                        }
                    }
                    ExterncItem::Cvar(cv) => {
                        let ty = cv.ty.to_string();
                        let fixed_size = parse_array_type(&ty).and_then(|a| a.size);
                        let vi = VarInfo {
                            ty,
                            is_static: false,
                            fixed_size,
                        };
                        global_vars.insert(cv.name.name.clone(), vi.clone());
                        if let Some(ref ns) = ext.namespace {
                            global_vars.insert(format!("{ns}::{}", cv.name.name), vi);
                        }
                    }
                }
            }
        }
        ItemKind::Class(c) => {
            let name = alias.unwrap_or(&c.name.name);
            let mut fields = HashMap::new();
            let mut static_fields = HashSet::new();
            for f in &c.fields {
                fields.insert(f.name.name.clone(), f.ty.to_string());
                if f.is_static {
                    static_fields.insert(f.name.name.clone());
                }
            }
            let mut methods = HashMap::new();
            for m in &c.methods {
                methods.insert(m.name.name.clone(), make_fn_sig(&m.name.name, &m.params, m.return_type.as_ref()));
            }
            class_defs.insert(name.to_string(), ClassInfo {
                name: name.to_string(),
                base_class: c.base_class.as_ref().map(|b| b.name.clone()),
                fields: fields.clone(),
                static_fields: static_fields.clone(),
                methods: methods.clone(),
            });
            if alias.is_some() {
                class_defs.insert(c.name.name.clone(), ClassInfo {
                    name: c.name.name.clone(),
                    base_class: c.base_class.as_ref().map(|b| b.name.clone()),
                    fields,
                    static_fields,
                    methods,
                });
            }
        }
        ItemKind::Struct(s) => {
            let name = alias.unwrap_or(&s.name.name);
            let fields = s.fields.iter().map(|f| StructFieldInfo {
                name: f.name.name.clone(),
                ty: f.ty.to_string(),
                has_default: f.default_val.is_some(),
            }).collect();
            let mut methods = HashMap::new();
            for m in &s.methods {
                methods.insert(m.name.name.clone(), make_fn_sig(&m.name.name, &m.params, m.return_type.as_ref()));
            }
            struct_defs.insert(name.to_string(), StructInfo {
                name: name.to_string(),
                fields,
                methods,
                is_union: false,
            });
        }
        ItemKind::Static(s) => {
            let name = alias.unwrap_or(&s.name.name);
            let ty = if let Some(ref t) = s.ty {
                t.to_string()
            } else {
                infer_static_init_type(&s.init)
            };
            let fixed_size = parse_array_type(&ty).and_then(|a| a.size);
            global_vars.insert(name.to_string(), VarInfo {
                ty,
                is_static: true,
                fixed_size,
            });
        }
        ItemKind::TypeAlias(ta) => {
            if let Some(s) = struct_defs.get(&ta.target.to_string()).cloned() {
                struct_defs.insert(ta.name.name.clone(), s);
            }
        }
        _ => {}
    }
}

fn make_cfun_sig(name: &str, cf: &CfunDecl) -> FnSigInfo {
    let mut params: Vec<ParamSig> = cf.params.iter().map(|p| ParamSig {
        name: p.name.name.clone(),
        ty: Some(p.ty.to_string()),
        has_default: false,
        is_variadic: false,
    }).collect();
    if cf.is_variadic {
        params.push(ParamSig {
            name: "...".to_string(),
            ty: None,
            has_default: false,
            is_variadic: true,
        });
    }
    FnSigInfo {
        name: name.to_string(),
        params,
        return_type: cf.return_type.as_ref().map(|t| t.to_string()),
    }
}

fn register_ns_aliases_for_item(
    item: &Item,
    ns: &str,
    out_vars: &mut HashMap<String, VarInfo>,
    fn_sigs: &mut HashMap<String, FnSigInfo>,
    struct_defs: &mut HashMap<String, StructInfo>,
    global_vars: &mut HashMap<String, VarInfo>,
) {
    match &item.kind {
        ItemKind::Static(s) => {
            if let Some(v) = global_vars.get(&s.name.name).cloned() {
                out_vars.insert(format!("{ns}::{}", s.name.name), v);
            }
        }
        ItemKind::Cfun(cf) => {
            if let Some(sig) = fn_sigs.get(&cf.name.name).cloned() {
                fn_sigs.insert(format!("{ns}::{}", cf.name.name), sig);
            }
        }
        ItemKind::Cstruct(cs) => {
            if let Some(s) = struct_defs.get(&cs.name.name).cloned() {
                struct_defs.insert(format!("{ns}::{}", cs.name.name), s);
            }
        }
        ItemKind::Cvar(cv) => {
            if let Some(v) = global_vars.get(&cv.name.name).cloned() {
                out_vars.insert(format!("{ns}::{}", cv.name.name), v);
            }
        }
        ItemKind::TypeAlias(ta) => {
            if let Some(s) = struct_defs.get(&ta.target.to_string()).cloned() {
                struct_defs.insert(ta.name.name.clone(), s.clone());
                struct_defs.insert(format!("{ns}::{}", ta.name.name), s);
            }
        }
        _ => {}
    }
}

fn get_item_name(item: &Item) -> Option<&str> {
    match &item.kind {
        ItemKind::Fun(f) => Some(&f.name.name),
        ItemKind::Cfun(cf) => Some(&cf.name.name),
        ItemKind::Cvar(cv) => Some(&cv.name.name),
        ItemKind::Class(c) => Some(&c.name.name),
        ItemKind::Struct(s) => Some(&s.name.name),
        ItemKind::Cstruct(cs) => Some(&cs.name.name),
        ItemKind::Static(s) => Some(&s.name.name),
        _ => None,
    }
}

fn make_fn_sig(name: &str, params: &[Param], ret: Option<&TypeNode>) -> FnSigInfo {
    let p_sigs = params.iter().map(|p| {
        let ty = p.ty.as_ref().map(|t| {
            let base = t.to_string();
            if p.is_ref && p.is_mut {
                format!("ref ~{base}")
            } else if p.is_ref {
                format!("ref {base}")
            } else {
                base
            }
        });
        ParamSig {
            name: p.name.name.clone(),
            ty,
            has_default: p.default_val.is_some(),
            is_variadic: p.is_variadic,
        }
    }).collect();
    FnSigInfo {
        name: name.to_string(),
        params: p_sigs,
        return_type: ret.map(|t| t.to_string()),
    }
}

fn infer_static_init_type(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Literal(LiteralValue::Int(_)) => "int64".to_string(),
        ExprKind::Literal(LiteralValue::Float(_)) => "float64".to_string(),
        ExprKind::Literal(LiteralValue::String(_)) => "string".to_string(),
        ExprKind::Literal(LiteralValue::Bool(_)) => "bool".to_string(),
        ExprKind::Literal(LiteralValue::Bytes(_)) => "bytes".to_string(),
        ExprKind::Literal(LiteralValue::TypedInt(_, s)) => match s {
            IntSuffix::I8 => "int8",
            IntSuffix::I16 => "int16",
            IntSuffix::I32 => "int32",
            IntSuffix::I64 => "int64",
            IntSuffix::U8 => "uint8",
            IntSuffix::U16 => "uint16",
            IntSuffix::U32 => "uint32",
            IntSuffix::U64 => "uint64",
        }.to_string(),
        ExprKind::StructInit(ref name, _) => name.name.clone(),
        ExprKind::ClassInit(ref callee, _) => {
            if let ExprKind::Ident(ref id) = callee.kind {
                id.name.clone()
            } else {
                "any".to_string()
            }
        }
        ExprKind::ArrayInit(ref items) => {
            if let Some(first) = items.first() {
                let inner = infer_static_init_type(first);
                format!("{inner}[]")
            } else {
                "any[]".to_string()
            }
        }
        _ => "any".to_string(),
    }
}

fn check_fun_like(
    name: &str,
    params: &[Param],
    declared_ret: Option<&str>,
    body: &Block,
    class_context: Option<&str>,
    fn_sigs: &HashMap<String, FnSigInfo>,
    struct_defs: &HashMap<String, StructInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    global_vars: &HashMap<String, VarInfo>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let mut local_vars = HashMap::new();
    for p in params {
        if let Some(ref ty) = p.ty {
            let base = ty.to_string();
            let ty_str = if p.is_ref && p.is_mut {
                format!("ref ~{base}")
            } else if p.is_ref {
                format!("ref {base}")
            } else {
                base
            };
            let fixed_size = parse_array_type(&ty_str).and_then(|a| a.size);
            local_vars.insert(p.name.name.clone(), VarInfo {
                ty: ty_str,
                is_static: p.is_ref && !p.is_mut,
                fixed_size,
            });
        }
    }
    if let Some(cls) = class_context {
        local_vars.insert("this".to_string(), VarInfo {
            ty: cls.to_string(),
            is_static: true,
            fixed_size: None,
        });
    }

    validate_return_statements(name, declared_ret, body, &local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);

    check_block_stmts(&body.stmts, &mut local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
}

fn validate_return_statements(
    name: &str,
    declared_ret: Option<&str>,
    body: &Block,
    local_vars: &HashMap<String, VarInfo>,
    global_vars: &HashMap<String, VarInfo>,
    fn_sigs: &HashMap<String, FnSigInfo>,
    struct_defs: &HashMap<String, StructInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    class_context: Option<&str>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let returns = collect_returns(body);

    if let Some(ret_ty) = declared_ret {
        if ret_ty != "void" {
            if returns.is_empty() {
                let range = doc.span_to_range(body.span.start, body.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0410".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: format!("Missing return statement: function '{name}' expects return of type '{ret_ty}'"),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            } else {
                for (span, ret_expr) in returns {
                    match ret_expr {
                        None => {
                            let range = doc.span_to_range(span.start, span.end);
                            diags.push(LspDiagnostic {
                                range,
                                severity: Some(LspSeverity::ERROR),
                                code: Some(NumberOrString::String("E0401".to_string())),
                                code_description: None,
                                source: Some("yukino".to_string()),
                                message: format!("Mismatched types: expected return value of type '{ret_ty}', found 'void'"),
                                related_information: None,
                                tags: None,
                                data: None,
                            });
                        }
                        Some(expr) => {
                            validate_array_literal(
                                &expr,
                                ret_ty,
                                local_vars,
                                global_vars,
                                fn_sigs,
                                struct_defs,
                                class_defs,
                                class_context,
                                doc,
                                diags,
                            );
                            if let Some(actual) = infer_expr_type(&expr, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                                if !is_type_compatible(ret_ty, &actual) {
                                    let range = doc.span_to_range(expr.span.start, expr.span.end);
                                    diags.push(LspDiagnostic {
                                        range,
                                        severity: Some(LspSeverity::ERROR),
                                        code: Some(NumberOrString::String("E0401".to_string())),
                                        code_description: None,
                                        source: Some("yukino".to_string()),
                                        message: format!("Mismatched types: expected return type '{ret_ty}', found '{actual}'"),
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
    } else {
        for (_, ret_expr) in returns {
            if let Some(expr) = ret_expr {
                let range = doc.span_to_range(expr.span.start, expr.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0401".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: format!("Function '{name}' with void return type cannot return a value"),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
        }
    }
}

fn collect_returns(block: &Block) -> Vec<(yukic_span::Span, Option<Expr>)> {
    let mut returns = Vec::new();
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Return(ref opt) => {
                returns.push((stmt.span, opt.clone()));
            }
            StmtKind::Expr(ref e) => {
                collect_returns_from_expr(e, &mut returns);
            }
            _ => {}
        }
    }
    returns
}

fn collect_returns_from_expr(expr: &Expr, returns: &mut Vec<(yukic_span::Span, Option<Expr>)>) {
    match &expr.kind {
        ExprKind::If { then_branch, elif_branches, else_branch, .. } => {
            returns.extend(collect_returns(then_branch));
            for (_, elif_b) in elif_branches {
                returns.extend(collect_returns(elif_b));
            }
            if let Some(ref el) = else_branch {
                returns.extend(collect_returns(el));
            }
        }
        ExprKind::Block(b) | ExprKind::RegionBlock(b) => {
            returns.extend(collect_returns(b));
        }
        ExprKind::Switch { arms, .. } => {
            for arm in arms {
                returns.extend(collect_returns(&arm.body));
            }
        }
        _ => {}
    }
}

fn check_block_stmts(
    stmts: &[Stmt],
    local_vars: &mut HashMap<String, VarInfo>,
    global_vars: &HashMap<String, VarInfo>,
    fn_sigs: &HashMap<String, FnSigInfo>,
    struct_defs: &HashMap<String, StructInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    class_context: Option<&str>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Set { target, ty, init } => {
                check_var_stmt(
                    None,
                    Some(target),
                    ty.as_ref(),
                    init,
                    false,
                    local_vars,
                    global_vars,
                    fn_sigs,
                    struct_defs,
                    class_defs,
                    class_context,
                    doc,
                    diags,
                );
            }
            StmtKind::Static(ref s) => {
                check_var_stmt(
                    Some(&s.name),
                    None,
                    s.ty.as_ref(),
                    &s.init,
                    true,
                    local_vars,
                    global_vars,
                    fn_sigs,
                    struct_defs,
                    class_defs,
                    class_context,
                    doc,
                    diags,
                );
            }
            StmtKind::Rewrite { target, value, .. } => {
                if let LValue::Ident(ref id) = target {
                    let is_static = local_vars.get(&id.name).map(|v| v.is_static)
                        .or_else(|| global_vars.get(&id.name).map(|v| v.is_static))
                        .unwrap_or(false);
                    if is_static {
                        let range = doc.span_to_range(stmt.span.start, stmt.span.end);
                        diags.push(LspDiagnostic {
                            range,
                            severity: Some(LspSeverity::ERROR),
                            code: Some(NumberOrString::String("E0503".to_string())),
                            code_description: None,
                            source: Some("yukino".to_string()),
                            message: format!("cannot reassign static constant '{}'", id.name),
                            related_information: None,
                            tags: None,
                            data: None,
                        });
                    }
                } else if let LValue::Field(ref obj, ref field) = target {
                    if let Some(obj_ty) = infer_expr_type(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                        if is_class_field_static(&obj_ty, &field.name, class_defs) {
                            let range = doc.span_to_range(stmt.span.start, stmt.span.end);
                            diags.push(LspDiagnostic {
                                range,
                                severity: Some(LspSeverity::ERROR),
                                code: Some(NumberOrString::String("E0503".to_string())),
                                code_description: None,
                                source: Some("yukino".to_string()),
                                message: format!("cannot reassign static field '{}'", field.name),
                                related_information: None,
                                tags: None,
                                data: None,
                            });
                        }
                    }
                }
                let target_ty = resolve_lvalue_type(target, local_vars, global_vars, class_defs, class_context);
                if let Some(ref expected_str) = target_ty {
                    let arr_checked = validate_array_literal(
                        value,
                        expected_str,
                        local_vars,
                        global_vars,
                        fn_sigs,
                        struct_defs,
                        class_defs,
                        class_context,
                        doc,
                        diags,
                    );
                    if !arr_checked {
                        if let Some(actual) = infer_expr_type(value, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                            if !is_type_compatible(expected_str, &actual) {
                                let range = doc.span_to_range(value.span.start, value.span.end);
                                diags.push(LspDiagnostic {
                                    range,
                                    severity: Some(LspSeverity::ERROR),
                                    code: Some(NumberOrString::String("E0401".to_string())),
                                    code_description: None,
                                    source: Some("yukino".to_string()),
                                    message: format!("Mismatched types: expected '{expected_str}', found '{actual}'"),
                                    related_information: None,
                                    tags: None,
                                    data: None,
                                });
                            }
                        }
                    }
                }
                check_expr(value, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
            StmtKind::Expr(ref e) => {
                check_expr(e, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
            StmtKind::Return(Some(ref e)) => {
                check_expr(e, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
            StmtKind::Defer(ref b) => {
                check_block_stmts(&b.stmts, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
            _ => {}
        }
    }
}

fn check_var_stmt(
    name: Option<&Ident>,
    target: Option<&BindingTarget>,
    ty: Option<&TypeNode>,
    init: &Expr,
    is_static: bool,
    local_vars: &mut HashMap<String, VarInfo>,
    global_vars: &HashMap<String, VarInfo>,
    fn_sigs: &HashMap<String, FnSigInfo>,
    struct_defs: &HashMap<String, StructInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    class_context: Option<&str>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let init_ty = infer_expr_type(init, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context);
    if let Some(declared_ty) = ty {
        let expected_str = declared_ty.to_string();
        let arr_checked = validate_array_literal(
            init,
            &expected_str,
            local_vars,
            global_vars,
            fn_sigs,
            struct_defs,
            class_defs,
            class_context,
            doc,
            diags,
        );
        if !arr_checked {
            if let Some(ref actual) = init_ty {
                if !is_type_compatible(&expected_str, actual) {
                    let range = doc.span_to_range(init.span.start, init.span.end);
                    diags.push(LspDiagnostic {
                        range,
                        severity: Some(LspSeverity::ERROR),
                        code: Some(NumberOrString::String("E0401".to_string())),
                        code_description: None,
                        source: Some("yukino".to_string()),
                        message: format!("Mismatched types: expected '{expected_str}', found '{actual}'"),
                        related_information: None,
                        tags: None,
                        data: None,
                    });
                }
            }
        }
        let fixed_size = parse_array_type(&expected_str).and_then(|a| a.size);
        if let Some(id) = name {
            local_vars.insert(id.name.clone(), VarInfo { ty: expected_str, is_static, fixed_size });
        } else if let Some(tgt) = target {
            bind_target_var_info(tgt, &expected_str, is_static, fixed_size, local_vars);
        }
    } else {
        if let ExprKind::ArrayInit(ref elems) = init.kind {
            if elems.is_empty() {
                let range = doc.span_to_range(init.span.start, init.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0402".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: "cannot infer type of empty array literal without context".to_string(),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
        }
        if let Some(actual) = init_ty {
            let fixed_size = parse_array_type(&actual).and_then(|a| a.size);
            if let Some(id) = name {
                local_vars.insert(id.name.clone(), VarInfo { ty: actual, is_static, fixed_size });
            } else if let Some(tgt) = target {
                bind_target_var_info(tgt, &actual, is_static, fixed_size, local_vars);
            }
        }
    }
    check_expr(init, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
}

fn validate_array_literal(
    init: &Expr,
    expected_ty_str: &str,
    local_vars: &HashMap<String, VarInfo>,
    global_vars: &HashMap<String, VarInfo>,
    fn_sigs: &HashMap<String, FnSigInfo>,
    struct_defs: &HashMap<String, StructInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    class_context: Option<&str>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) -> bool {
    let Some(arr_info) = parse_array_type(expected_ty_str) else {
        return false;
    };
    let ExprKind::ArrayInit(ref elems) = init.kind else {
        return false;
    };

    if let Some(expected_size) = arr_info.size {
        if elems.len() > expected_size {
            let range = doc.span_to_range(init.span.start, init.span.end);
            diags.push(LspDiagnostic {
                range,
                severity: Some(LspSeverity::ERROR),
                code: Some(NumberOrString::String("E0401".to_string())),
                code_description: None,
                source: Some("yukino".to_string()),
                message: format!(
                    "fixed-size array size mismatch: expected at most {} elements, found {}",
                    expected_size,
                    elems.len()
                ),
                related_information: None,
                tags: None,
                data: None,
            });
        } else if elems.len() < expected_size {
            let remaining = expected_size - elems.len();
            let range = doc.span_to_range(init.span.start, init.span.end);
            diags.push(LspDiagnostic {
                range,
                severity: Some(LspSeverity::WARNING),
                code: Some(NumberOrString::String("W0404".to_string())),
                code_description: None,
                source: Some("yukino".to_string()),
                message: format!(
                    "fixed-size array is partially initialized: remaining {} element(s) are zero-initialized",
                    remaining
                ),
                related_information: None,
                tags: None,
                data: None,
            });
        }
    }

    for elem in elems {
        if let Some(actual_elem_ty) = infer_expr_type(elem, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
            if !is_type_compatible(&arr_info.elem_ty, &actual_elem_ty) {
                let range = doc.span_to_range(elem.span.start, elem.span.end);
                diags.push(LspDiagnostic {
                    range,
                    severity: Some(LspSeverity::ERROR),
                    code: Some(NumberOrString::String("E0401".to_string())),
                    code_description: None,
                    source: Some("yukino".to_string()),
                    message: format!("Mismatched types: expected '{}', found '{}'", arr_info.elem_ty, actual_elem_ty),
                    related_information: None,
                    tags: None,
                    data: None,
                });
            }
        }
    }

    true
}

fn check_expr(
    expr: &Expr,
    local_vars: &HashMap<String, VarInfo>,
    global_vars: &HashMap<String, VarInfo>,
    fn_sigs: &HashMap<String, FnSigInfo>,
    struct_defs: &HashMap<String, StructInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    class_context: Option<&str>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    match &expr.kind {
        ExprKind::Call(callee, args) => {
            match &callee.kind {
                ExprKind::Ident(id) => {
                    if let Some(sig) = fn_sigs.get(&id.name) {
                        validate_call_args(&id.name, sig, args, expr.span, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
                    }
                }
                ExprKind::Path(segments) if !segments.is_empty() => {
                    let qualified = segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("::");
                    let short = &segments.last().unwrap().name;
                    if let Some(sig) = fn_sigs.get(&qualified).or_else(|| fn_sigs.get(short)) {
                        validate_call_args(&qualified, sig, args, expr.span, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
                    }
                }
                _ => {}
            }
            check_expr(callee, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            for a in args {
                check_expr(a, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
        }
        ExprKind::ClassInit(callee, args) => {
            if let ExprKind::Ident(ref id) = callee.kind {
                if let Some(_cls) = class_defs.get(&id.name) {
                    if let Some(init_sig) = find_class_method(&id.name, "init", class_defs) {
                        validate_call_args(&format!("{}.init", id.name), init_sig, args, expr.span, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
                    } else if !args.is_empty() {
                        let range = doc.span_to_range(expr.span.start, expr.span.end);
                        diags.push(LspDiagnostic {
                            range,
                            severity: Some(LspSeverity::ERROR),
                            code: Some(NumberOrString::String("E0401".to_string())),
                            code_description: None,
                            source: Some("yukino".to_string()),
                            message: format!("class '{}' has no constructor and expects 0 arguments, found {}", id.name, args.len()),
                            related_information: None,
                            tags: None,
                            data: None,
                        });
                    }
                }
            }
            check_expr(callee, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            for a in args {
                check_expr(a, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
        }
        ExprKind::StructInit(name, fields) => {
            validate_struct_init(name, fields, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            for f in fields {
                check_expr(&f.value, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
        }
        ExprKind::Ref { mutable, expr: ref inner } => {
            if *mutable {
                if let ExprKind::Ident(ref id) = inner.kind {
                    if let Some(v) = local_vars.get(&id.name).or_else(|| global_vars.get(&id.name)) {
                        if v.is_static {
                            let range = doc.span_to_range(expr.span.start, expr.span.end);
                            diags.push(LspDiagnostic {
                                range,
                                severity: Some(LspSeverity::ERROR),
                                code: Some(NumberOrString::String("E0503".to_string())),
                                code_description: None,
                                source: Some("yukino".to_string()),
                                message: format!("Cannot borrow immutable static variable '{}' as mutable", id.name),
                                related_information: None,
                                tags: None,
                                data: None,
                            });
                        }
                    }
                }
            }
            check_expr(inner, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
        }
        ExprKind::Binary(_, l, r) => {
            check_expr(l, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            check_expr(r, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
        }
        ExprKind::Unary(_, inner) => {
            check_expr(inner, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
        }
        ExprKind::MethodCall(obj, method_name, args) => {
            if let Some(obj_ty) = infer_expr_type(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                let clean_ty = obj_ty.trim_end_matches('?');
                if let Some(arr_info) = parse_array_type(clean_ty) {
                    let is_std_method = matches!(
                        method_name.name.as_str(),
                        "len" | "cap" | "is_empty" | "as_ptr" | "cp" | "export" | "persistent" | "pop" | "push" | "del" | "clear" | "reserve"
                    );
                    if !is_std_method {
                        let range = doc.span_to_range(method_name.span.start, method_name.span.end);
                        diags.push(LspDiagnostic {
                            range,
                            severity: Some(LspSeverity::ERROR),
                            code: Some(NumberOrString::String("E0403".to_string())),
                            code_description: None,
                            source: Some("yukino".to_string()),
                            message: format!("No method named '{}' found on array type '{}'", method_name.name, clean_ty),
                            related_information: None,
                            tags: None,
                            data: None,
                        });
                    } else if method_name.name == "push" {
                        if let Some(arg) = args.first() {
                            validate_array_literal(arg, &arr_info.elem_ty, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
                            if let Some(act_ty) = infer_expr_type(arg, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                                if !is_type_compatible(&arr_info.elem_ty, &act_ty) {
                                    let range = doc.span_to_range(arg.span.start, arg.span.end);
                                    diags.push(LspDiagnostic {
                                        range,
                                        severity: Some(LspSeverity::ERROR),
                                        code: Some(NumberOrString::String("E0401".to_string())),
                                        code_description: None,
                                        source: Some("yukino".to_string()),
                                        message: format!("Mismatched types: expected '{}', found '{}' in 'push'", arr_info.elem_ty, act_ty),
                                        related_information: None,
                                        tags: None,
                                        data: None,
                                    });
                                }
                            }
                        }
                    }
                } else if let Some(cls) = class_defs.get(clean_ty) {
                    if let Some(sig) = find_class_method(clean_ty, &method_name.name, class_defs) {
                        validate_call_args(&method_name.name, sig, args, expr.span, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
                    } else if method_name.name != "init" && (!cls.methods.is_empty() || !cls.fields.is_empty()) && !has_unknown_base(clean_ty, class_defs) {
                        let range = doc.span_to_range(method_name.span.start, method_name.span.end);
                        diags.push(LspDiagnostic {
                            range,
                            severity: Some(LspSeverity::ERROR),
                            code: Some(NumberOrString::String("E0403".to_string())),
                            code_description: None,
                            source: Some("yukino".to_string()),
                            message: format!("No method named '{}' found on class '{}'", method_name.name, clean_ty),
                            related_information: None,
                            tags: None,
                            data: None,
                        });
                    }
                } else if let Some(st) = struct_defs.get(clean_ty) {
                    if let Some(sig) = st.methods.get(&method_name.name) {
                        validate_call_args(&method_name.name, sig, args, expr.span, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
                    } else if !st.methods.is_empty() || !st.fields.is_empty() {
                        let range = doc.span_to_range(method_name.span.start, method_name.span.end);
                        diags.push(LspDiagnostic {
                            range,
                            severity: Some(LspSeverity::ERROR),
                            code: Some(NumberOrString::String("E0403".to_string())),
                            code_description: None,
                            source: Some("yukino".to_string()),
                            message: format!("No method named '{}' found on struct '{}'", method_name.name, clean_ty),
                            related_information: None,
                            tags: None,
                            data: None,
                        });
                    }
                }
            }
            check_expr(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            for a in args {
                check_expr(a, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
        }
        ExprKind::IndexAccess(obj, idx) => {
            check_expr(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            check_expr(idx, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            if let Some(obj_ty) = infer_expr_type(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                let arr_info = parse_array_type(&obj_ty);
                let is_indexable = arr_info.is_some() || obj_ty == "string" || obj_ty == "bytes" || obj_ty == "any";
                if !is_indexable {
                    let range = doc.span_to_range(expr.span.start, expr.span.end);
                    diags.push(LspDiagnostic {
                        range,
                        severity: Some(LspSeverity::ERROR),
                        code: Some(NumberOrString::String("E0401".to_string())),
                        code_description: None,
                        source: Some("yukino".to_string()),
                        message: format!("type '{}' cannot be indexed", obj_ty),
                        related_information: None,
                        tags: None,
                        data: None,
                    });
                }
                if let Some(idx_ty) = infer_expr_type(idx, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                    let is_int = matches!(idx_ty.as_str(), "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64" | "any");
                    if !is_int {
                        let range = doc.span_to_range(idx.span.start, idx.span.end);
                        diags.push(LspDiagnostic {
                            range,
                            severity: Some(LspSeverity::ERROR),
                            code: Some(NumberOrString::String("E0401".to_string())),
                            code_description: None,
                            source: Some("yukino".to_string()),
                            message: format!("expected integer index, found '{}'", idx_ty),
                            related_information: None,
                            tags: None,
                            data: None,
                        });
                    }
                }
                if let Some(info) = arr_info {
                    if let Some(size) = info.size {
                        let idx_val = match &idx.kind {
                            ExprKind::Literal(LiteralValue::Int(n)) => Some(*n),
                            ExprKind::Literal(LiteralValue::UInt(n)) => Some(*n as i64),
                            ExprKind::Literal(LiteralValue::TypedInt(n, _)) => Some(*n),
                            ExprKind::Literal(LiteralValue::TypedUInt(n, _)) => Some(*n as i64),
                            _ => None,
                        };
                        if let Some(val) = idx_val {
                            if val < 0 || (val as usize) >= size {
                                let range = doc.span_to_range(idx.span.start, idx.span.end);
                                diags.push(LspDiagnostic {
                                    range,
                                    severity: Some(LspSeverity::ERROR),
                                    code: Some(NumberOrString::String("E0401".to_string())),
                                    code_description: None,
                                    source: Some("yukino".to_string()),
                                    message: format!("array index out of bounds: index is {val}, but array length is {size}"),
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
        ExprKind::SliceAccess(obj, s, e, _) => {
            check_expr(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            if let Some(ref st) = s {
                check_expr(st, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
            if let Some(ref ed) = e {
                check_expr(ed, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
            if let Some(obj_ty) = infer_expr_type(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                let arr_info = parse_array_type(&obj_ty);
                let is_sliceable = arr_info.is_some() || obj_ty == "string" || obj_ty == "bytes" || obj_ty == "any";
                if !is_sliceable {
                    let range = doc.span_to_range(expr.span.start, expr.span.end);
                    diags.push(LspDiagnostic {
                        range,
                        severity: Some(LspSeverity::ERROR),
                        code: Some(NumberOrString::String("E0401".to_string())),
                        code_description: None,
                        source: Some("yukino".to_string()),
                        message: format!("type '{}' cannot be sliced", obj_ty),
                        related_information: None,
                        tags: None,
                        data: None,
                    });
                }
            }
        }
        ExprKind::FieldAccess(obj, _) => {
            check_expr(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
        }
        ExprKind::If { cond, then_branch, elif_branches, else_branch } => {
            check_expr(cond, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            let mut branch_vars = local_vars.clone();
            check_block_stmts(&then_branch.stmts, &mut branch_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            for (c, b) in elif_branches {
                check_expr(c, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
                let mut elif_vars = local_vars.clone();
                check_block_stmts(&b.stmts, &mut elif_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
            if let Some(el) = else_branch {
                let mut else_vars = local_vars.clone();
                check_block_stmts(&el.stmts, &mut else_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context, doc, diags);
            }
        }
        _ => {}
    }
}

fn validate_call_args(
    fn_name: &str,
    sig: &FnSigInfo,
    args: &[Expr],
    call_span: yukic_span::Span,
    local_vars: &HashMap<String, VarInfo>,
    global_vars: &HashMap<String, VarInfo>,
    fn_sigs: &HashMap<String, FnSigInfo>,
    struct_defs: &HashMap<String, StructInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    class_context: Option<&str>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let min_params = sig.params.iter().filter(|p| !p.has_default && !p.is_variadic).count();
    let has_variadic = sig.params.iter().any(|p| p.is_variadic);
    let max_params = if has_variadic { usize::MAX } else { sig.params.len() };

    if args.len() < min_params || args.len() > max_params {
        let range = doc.span_to_range(call_span.start, call_span.end);
        diags.push(LspDiagnostic {
            range,
            severity: Some(LspSeverity::ERROR),
            code: Some(NumberOrString::String("E0401".to_string())),
            code_description: None,
            source: Some("yukino".to_string()),
            message: format!(
                "Function '{fn_name}' takes {} argument(s) but {} were supplied",
                if min_params == max_params { format!("{min_params}") } else { format!("{min_params}..{max_params}") },
                args.len()
            ),
            related_information: None,
            tags: None,
            data: None,
        });
    }

    for (i, arg) in args.iter().enumerate() {
        if i < sig.params.len() {
            let p = &sig.params[i];
            if let Some(ref exp_ty) = p.ty {
                let arr_checked = validate_array_literal(
                    arg,
                    exp_ty,
                    local_vars,
                    global_vars,
                    fn_sigs,
                    struct_defs,
                    class_defs,
                    class_context,
                    doc,
                    diags,
                );
                if !arr_checked {
                    if let Some(act_ty) = infer_expr_type(arg, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                        if !is_type_compatible(exp_ty, &act_ty) {
                            let range = doc.span_to_range(arg.span.start, arg.span.end);
                            diags.push(LspDiagnostic {
                                range,
                                severity: Some(LspSeverity::ERROR),
                                code: Some(NumberOrString::String("E0401".to_string())),
                                code_description: None,
                                source: Some("yukino".to_string()),
                                message: format!("Mismatched types: expected '{}', found '{}' for argument '{}'", exp_ty, act_ty, p.name),
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

fn validate_struct_init(
    name: &Ident,
    fields: &[FieldInit],
    local_vars: &HashMap<String, VarInfo>,
    global_vars: &HashMap<String, VarInfo>,
    fn_sigs: &HashMap<String, FnSigInfo>,
    struct_defs: &HashMap<String, StructInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    class_context: Option<&str>,
    doc: &Document,
    diags: &mut Vec<LspDiagnostic>,
) {
    let Some(s_def) = struct_defs.get(&name.name) else { return; };
    let mut seen_fields = HashSet::new();

    for f in fields {
        seen_fields.insert(f.name.name.clone());
        if let Some(f_def) = s_def.fields.iter().find(|fd| fd.name == f.name.name) {
            let arr_checked = validate_array_literal(
                &f.value,
                &f_def.ty,
                local_vars,
                global_vars,
                fn_sigs,
                struct_defs,
                class_defs,
                class_context,
                doc,
                diags,
            );
            if !arr_checked {
                if let Some(actual) = infer_expr_type(&f.value, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                    if !is_type_compatible(&f_def.ty, &actual) {
                        let range = doc.span_to_range(f.value.span.start, f.value.span.end);
                        diags.push(LspDiagnostic {
                            range,
                            severity: Some(LspSeverity::ERROR),
                            code: Some(NumberOrString::String("E0401".to_string())),
                            code_description: None,
                            source: Some("yukino".to_string()),
                            message: format!("Mismatched types: field '{}' of struct '{}' expects '{}', found '{}'", f.name.name, name.name, f_def.ty, actual),
                            related_information: None,
                            tags: None,
                            data: None,
                        });
                    }
                }
            }
        } else {
            let range = doc.span_to_range(f.name.span.start, f.name.span.end);
            diags.push(LspDiagnostic {
                range,
                severity: Some(LspSeverity::ERROR),
                code: Some(NumberOrString::String("E0305".to_string())),
                code_description: None,
                source: Some("yukino".to_string()),
                message: format!("Struct '{}' has no field named '{}'", name.name, f.name.name),
                related_information: None,
                tags: None,
                data: None,
            });
        }
    }

    if s_def.is_union {
        if fields.len() > 1 {
            let range = doc.span_to_range(name.span.start, name.span.end);
            diags.push(LspDiagnostic {
                range,
                severity: Some(LspSeverity::ERROR),
                code: Some(NumberOrString::String("E0401".to_string())),
                code_description: None,
                source: Some("yukino".to_string()),
                message: format!("C union '{}' can be initialized with at most 1 field, found {}", name.name, fields.len()),
                related_information: None,
                tags: None,
                data: None,
            });
        }
        return;
    }

    for req in &s_def.fields {
        if !req.has_default && !seen_fields.contains(&req.name) {
            let range = doc.span_to_range(name.span.start, name.span.end);
            diags.push(LspDiagnostic {
                range,
                severity: Some(LspSeverity::ERROR),
                code: Some(NumberOrString::String("E0401".to_string())),
                code_description: None,
                source: Some("yukino".to_string()),
                message: format!("Missing field '{}' in initializer of struct '{}'", req.name, name.name),
                related_information: None,
                tags: None,
                data: None,
            });
        }
    }
}

pub fn infer_expr_type(
    expr: &Expr,
    local_vars: &HashMap<String, VarInfo>,
    global_vars: &HashMap<String, VarInfo>,
    fn_sigs: &HashMap<String, FnSigInfo>,
    struct_defs: &HashMap<String, StructInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    class_context: Option<&str>,
) -> Option<String> {
    match &expr.kind {
        ExprKind::Literal(ref lit) => match lit {
            LiteralValue::Int(_) => Some("int64".to_string()),
            LiteralValue::UInt(_) => Some("uint64".to_string()),
            LiteralValue::Float(_) => Some("float64".to_string()),
            LiteralValue::TypedInt(_, s) | LiteralValue::TypedUInt(_, s) => Some(match s {
                IntSuffix::I8 => "int8",
                IntSuffix::I16 => "int16",
                IntSuffix::I32 => "int32",
                IntSuffix::I64 => "int64",
                IntSuffix::U8 => "uint8",
                IntSuffix::U16 => "uint16",
                IntSuffix::U32 => "uint32",
                IntSuffix::U64 => "uint64",
            }.to_string()),
            LiteralValue::TypedFloat(_, s) => Some(match s {
                FloatSuffix::F32 => "float32",
                FloatSuffix::F64 => "float64",
            }.to_string()),
            LiteralValue::String(_) => Some("string".to_string()),
            LiteralValue::Char(_) => Some("char".to_string()),
            LiteralValue::Bool(_) => Some("bool".to_string()),
            LiteralValue::Bytes(_) => Some("bytes".to_string()),
            LiteralValue::Null => Some("null".to_string()),
        },
        ExprKind::InterpolatedString(_) => Some("string".to_string()),
        ExprKind::Ident(ref id) => {
            if let Some(v) = local_vars.get(&id.name) {
                let ty = if let Some(inner) = v.ty.strip_prefix("ref ~").or_else(|| v.ty.strip_prefix("ref ")) {
                    inner.trim().to_string()
                } else {
                    v.ty.clone()
                };
                Some(ty)
            } else if let Some(v) = global_vars.get(&id.name) {
                let ty = if let Some(inner) = v.ty.strip_prefix("ref ~").or_else(|| v.ty.strip_prefix("ref ")) {
                    inner.trim().to_string()
                } else {
                    v.ty.clone()
                };
                Some(ty)
            } else if id.name == "true" || id.name == "false" {
                Some("bool".to_string())
            } else if id.name == "null" {
                Some("null".to_string())
            } else if id.name == "errkind" {
                Some("int32".to_string())
            } else if id.name == "space" {
                Some("void".to_string())
            } else if struct_defs.contains_key(&id.name) {
                Some(id.name.clone())
            } else if class_defs.contains_key(&id.name) {
                Some(id.name.clone())
            } else {
                None
            }
        }
        ExprKind::Path(ref segments) if !segments.is_empty() => {
            let qualified = segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("::");
            let last_name = &segments.last().unwrap().name;
            if let Some(v) = global_vars.get(&qualified).or_else(|| global_vars.get(last_name)) {
                let ty = if let Some(inner) = v.ty.strip_prefix("ref ~").or_else(|| v.ty.strip_prefix("ref ")) {
                    inner.trim().to_string()
                } else {
                    v.ty.clone()
                };
                Some(ty)
            } else if struct_defs.contains_key(&qualified) {
                Some(qualified)
            } else if struct_defs.contains_key(last_name) {
                Some(last_name.clone())
            } else if class_defs.contains_key(last_name) {
                Some(last_name.clone())
            } else {
                None
            }
        }
        ExprKind::This => class_context.map(|s| s.to_string()),
        ExprKind::Super => class_context.and_then(|s| {
            class_defs.get(s).and_then(|c| c.base_class.clone())
        }),
        ExprKind::FieldAccess(ref obj, ref field) => {
            let obj_ty = infer_expr_type(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context)?;
            if let Some(c_info) = class_defs.get(&obj_ty) {
                c_info.fields.get(&field.name).cloned()
            } else if let Some(s_info) = struct_defs.get(&obj_ty) {
                s_info.fields.iter().find(|f| f.name == field.name).map(|f| f.ty.clone())
            } else {
                None
            }
        }
        ExprKind::StructInit(ref name, _) => Some(name.name.clone()),
        ExprKind::ClassInit(ref callee, _) => {
            if let ExprKind::Ident(ref id) = callee.kind {
                Some(id.name.clone())
            } else {
                None
            }
        }
        ExprKind::ArrayInit(ref items) => {
            if items.is_empty() {
                Some("any[]".to_string())
            } else {
                let mut elem_types = Vec::new();
                for item in items {
                    if let Some(t) = infer_expr_type(item, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                        elem_types.push(t);
                    }
                }
                if elem_types.is_empty() {
                    Some("any[]".to_string())
                } else {
                    let first = &elem_types[0];
                    if elem_types.iter().all(|t| is_type_compatible(first, t)) {
                        Some(format!("{first}[]"))
                    } else {
                        let is_int = |t: &str| matches!(t, "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64");
                        if elem_types.iter().all(|t| is_int(t)) {
                            Some("int64[]".to_string())
                        } else {
                            Some("any[]".to_string())
                        }
                    }
                }
            }
        }
        ExprKind::ArrayAlloc(ref elem_ty, _) => Some(format!("{elem_ty}[]")),
        ExprKind::IndexAccess(ref obj, _) => {
            let obj_ty = infer_expr_type(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context)?;
            if let Some(arr_info) = parse_array_type(&obj_ty) {
                Some(arr_info.elem_ty)
            } else if obj_ty == "string" || obj_ty == "bytes" {
                Some("uint8".to_string())
            } else {
                None
            }
        }
        ExprKind::SliceAccess(ref obj, _, _, _) => {
            let obj_ty = infer_expr_type(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context)?;
            if let Some(arr_info) = parse_array_type(&obj_ty) {
                Some(format!("{}[]", arr_info.elem_ty))
            } else if obj_ty == "string" {
                Some("string".to_string())
            } else if obj_ty == "bytes" {
                Some("bytes".to_string())
            } else {
                None
            }
        }
        ExprKind::Binary(op, ref l, ref r) => match op {
            BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq
            | BinaryOp::And | BinaryOp::Or => Some("bool".to_string()),
            _ => infer_expr_type(l, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context)
                .or_else(|| infer_expr_type(r, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context)),
        },
        ExprKind::Ref { mutable, ref expr } => {
            let inner_ty = infer_expr_type(expr, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context)?;
            if *mutable {
                Some(format!("ref ~{inner_ty}"))
            } else {
                Some(format!("ref {inner_ty}"))
            }
        }
        ExprKind::Unary(op, ref inner) => match op {
            UnaryOp::Not => Some("bool".to_string()),
            _ => infer_expr_type(inner, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context),
        },
        ExprKind::Call(ref callee, ref args) => {
            if let ExprKind::Ident(ref id) = callee.kind {
                if id.name == "cast" && args.len() >= 2 {
                    if let ExprKind::Literal(LiteralValue::String(ref s)) = args[1].kind {
                        return Some(s.clone());
                    }
                    if let Some(t) = infer_expr_type(&args[1], local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                        return Some(t);
                    }
                }
                if id.name == "typeof!" {
                    return Some("string".to_string());
                }
                if id.name == "ptr_of" && !args.is_empty() {
                    let inner = infer_expr_type(&args[0], local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context).unwrap_or_else(|| "any".to_string());
                    return Some(format!("ptr<{inner}>"));
                }
                if (id.name == "persistent" || id.name == "export") && !args.is_empty() {
                    if let Some(inner) = infer_expr_type(&args[0], local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                        return Some(inner);
                    }
                }
                if id.name == "from_raw_parts" && !args.is_empty() {
                    let p_ty = infer_expr_type(&args[0], local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context).unwrap_or_default();
                    if let Some(elem) = p_ty.strip_prefix("ptr<").and_then(|s| s.strip_suffix('>')) {
                        return Some(format!("{elem}[]?"));
                    }
                }
                if let Some(sig) = fn_sigs.get(&id.name) {
                    if sig.return_type.as_deref() == Some("T") {
                        if let Some(idx) = sig.params.iter().position(|p| p.ty.as_deref() == Some("T")) {
                            if let Some(arg) = args.get(idx) {
                                if let Some(arg_ty) = infer_expr_type(arg, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context) {
                                    return Some(arg_ty);
                                }
                            }
                        }
                    }
                    return sig.return_type.clone();
                }
            } else if let ExprKind::Path(ref segments) = callee.kind {
                if !segments.is_empty() {
                    let qualified = segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("::");
                    let short = &segments.last().unwrap().name;
                    if let Some(sig) = fn_sigs.get(&qualified).or_else(|| fn_sigs.get(short)) {
                        return sig.return_type.clone();
                    }
                }
            }
            None
        }
        ExprKind::MethodCall(ref obj, ref method, _) => {
            let obj_ty = infer_expr_type(obj, local_vars, global_vars, fn_sigs, struct_defs, class_defs, class_context)?;
            if let Some(arr_info) = parse_array_type(&obj_ty) {
                return match method.name.as_str() {
                    "len" | "cap" => Some("int64".to_string()),
                    "is_empty" => Some("bool".to_string()),
                    "as_ptr" => Some(format!("ptr<{}>", arr_info.elem_ty)),
                    "cp" | "export" | "persistent" => Some(format!("{}[]", arr_info.elem_ty)),
                    "pop" => Some(format!("{}?", arr_info.elem_ty)),
                    "push" | "del" | "clear" | "reserve" => Some("void".to_string()),
                    _ => None,
                };
            }
            let clean_ty = obj_ty.trim_end_matches('?');
            if class_defs.contains_key(clean_ty) {
                if let Some(m_sig) = find_class_method(clean_ty, &method.name, class_defs) {
                    return m_sig.return_type.clone();
                }
            } else if let Some(s_info) = struct_defs.get(clean_ty) {
                if let Some(m_sig) = s_info.methods.get(&method.name) {
                    return m_sig.return_type.clone();
                }
            }
            None
        }
        _ => None,
    }
}

pub fn is_type_compatible(expected: &str, actual: &str) -> bool {
    let exp = expected.trim();
    let act = actual.trim();
    if exp == act || exp == "any" || act == "any" || exp.is_empty() || act.is_empty() {
        return true;
    }

    let is_generic_param = |t: &str| {
        t == "T" || t == "U" || t == "K" || t == "V" || t == "E" || t == "R"
            || (t.len() == 1 && t.chars().next().map_or(false, |c| c.is_ascii_uppercase()))
    };

    if is_generic_param(exp) || is_generic_param(act) {
        return true;
    }

    if let Some(base) = exp.strip_suffix('?') {
        if act == "null" || is_type_compatible(base, act) {
            return true;
        }
    }
    if exp.contains('|') {
        for part in exp.split('|') {
            if is_type_compatible(part.trim(), act) {
                return true;
            }
        }
    }
    let exp_mut_ref = exp.strip_prefix("ref ~");
    let exp_imm_ref = if exp_mut_ref.is_none() { exp.strip_prefix("ref ") } else { None };
    let act_mut_ref = act.strip_prefix("ref ~");
    let act_imm_ref = if act_mut_ref.is_none() { act.strip_prefix("ref ") } else { None };

    if let Some(exp_inner) = exp_mut_ref {
        if let Some(act_inner) = act_mut_ref {
            return is_type_compatible(exp_inner, act_inner);
        }
        return false;
    }
    if let Some(exp_inner) = exp_imm_ref {
        if let Some(act_inner) = act_imm_ref.or(act_mut_ref) {
            return is_type_compatible(exp_inner, act_inner);
        }
        return false;
    }
    if act_mut_ref.is_some() || act_imm_ref.is_some() {
        return false;
    }
    if let (Some(exp_arr), Some(act_arr)) = (parse_array_type(exp), parse_array_type(act)) {
        if exp_arr.elem_ty == "any" || act_arr.elem_ty == "any"
            || is_generic_param(&exp_arr.elem_ty) || is_generic_param(&act_arr.elem_ty)
        {
            return true;
        }
        if let (Some(exp_sz), Some(act_sz)) = (exp_arr.size, act_arr.size) {
            if exp_sz != act_sz {
                return false;
            }
        }
        return is_type_compatible(&exp_arr.elem_ty, &act_arr.elem_ty);
    }
    if exp.starts_with("ptr<") && exp.ends_with('>') && act.starts_with("ptr<") && act.ends_with('>') {
        let exp_inner = &exp[4..exp.len() - 1];
        let act_inner = &act[4..act.len() - 1];
        if is_generic_param(exp_inner) || is_generic_param(act_inner) || is_type_compatible(exp_inner, act_inner) {
            return true;
        }
    }
    let is_int = |t: &str| matches!(
        t,
        "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64"
            | "c_char" | "c_schar" | "c_uchar" | "c_short" | "c_ushort"
            | "c_int" | "c_uint" | "c_long" | "c_ulong" | "c_longlong" | "c_ulonglong"
            | "c_size_t" | "c_ssize_t" | "usize" | "isize"
    );
    if is_int(exp) && is_int(act) {
        return true;
    }
    if (exp == "bool" || exp == "c_bool") && (act == "bool" || act == "c_bool") {
        return true;
    }
    if exp.starts_with("cfun(") && act.starts_with("cfun(") {
        return true;
    }
    let is_float = |t: &str| matches!(t, "float32" | "float64");
    if is_float(exp) && is_float(act) {
        return true;
    }
    if let Some(inner) = exp.strip_prefix("ref ").or_else(|| exp.strip_prefix("ref ~")) {
        if is_type_compatible(inner.trim(), act) {
            return true;
        }
    }
    false
}

fn resolve_lvalue_type(
    lval: &LValue,
    local_vars: &HashMap<String, VarInfo>,
    global_vars: &HashMap<String, VarInfo>,
    class_defs: &HashMap<String, ClassInfo>,
    class_context: Option<&str>,
) -> Option<String> {
    match lval {
        LValue::Ident(ref id) => {
            let ty = local_vars.get(&id.name).map(|v| v.ty.clone())
                .or_else(|| global_vars.get(&id.name).map(|v| v.ty.clone()))?;
            if let Some(inner) = ty.strip_prefix("ref ~").or_else(|| ty.strip_prefix("ref ")) {
                return Some(inner.trim().to_string());
            }
            Some(ty)
        }
        LValue::Field(ref obj, ref field) => {
            if let ExprKind::This = obj.kind {
                if let Some(cls) = class_context {
                    if let Some(c_info) = class_defs.get(cls) {
                        return c_info.fields.get(&field.name).cloned();
                    }
                }
            }
            None
        }
        LValue::Index(ref target_expr, _) => {
            let target_ty = infer_expr_type(target_expr, local_vars, global_vars, &HashMap::new(), &HashMap::new(), class_defs, class_context)?;
            if let Some(arr_info) = parse_array_type(&target_ty) {
                return Some(arr_info.elem_ty);
            }
            if target_ty == "bytes" || target_ty == "string" {
                return Some("uint8".to_string());
            }
            None
        }
    }
}

fn bind_target_var_info(
    target: &BindingTarget,
    ty: &str,
    is_static: bool,
    fixed_size: Option<usize>,
    local_vars: &mut HashMap<String, VarInfo>,
) {
    match target {
        BindingTarget::Ident(ref id) => {
            local_vars.insert(
                id.name.clone(),
                VarInfo {
                    ty: ty.to_string(),
                    is_static,
                    fixed_size,
                },
            );
        }
        BindingTarget::Array(ref elems) => {
            let elem_ty = parse_array_type(ty).map(|a| a.elem_ty).unwrap_or_else(|| "any".to_string());
            for elem in elems {
                if elem.is_rest {
                    bind_target_var_info(&elem.target, &format!("{elem_ty}[]"), is_static, None, local_vars);
                } else {
                    bind_target_var_info(&elem.target, &elem_ty, is_static, None, local_vars);
                }
            }
        }
        BindingTarget::Tuple(ref targets) => {
            for t in targets {
                bind_target_var_info(t, "any", is_static, None, local_vars);
            }
        }
        BindingTarget::Map(_) => {}
    }
}

fn find_class_method<'a>(
    cls_name: &str,
    method_name: &str,
    class_defs: &'a HashMap<String, ClassInfo>,
) -> Option<&'a FnSigInfo> {
    let mut current = Some(cls_name);
    let mut depth = 0;
    while let Some(name) = current {
        if depth > 10 {
            break;
        }
        depth += 1;
        let cls = class_defs.get(name)?;
        if let Some(sig) = cls.methods.get(method_name) {
            return Some(sig);
        }
        current = cls.base_class.as_deref();
    }
    None
}

fn has_unknown_base(cls_name: &str, class_defs: &HashMap<String, ClassInfo>) -> bool {
    let mut current = Some(cls_name);
    let mut depth = 0;
    while let Some(name) = current {
        if depth > 10 {
            return true;
        }
        depth += 1;
        let Some(cls) = class_defs.get(name) else {
            return true;
        };
        let Some(ref base) = cls.base_class else {
            return false;
        };
        if !class_defs.contains_key(base) {
            return true;
        }
        current = Some(base);
    }
    false
}

fn is_literal_expr(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Literal(_) => true,
        ExprKind::Unary(UnaryOp::Neg | UnaryOp::Not | UnaryOp::BitNot, inner) => {
            is_literal_expr(inner)
        }
        _ => false,
    }
}

fn is_class_field_static(
    cls_name: &str,
    field_name: &str,
    class_defs: &HashMap<String, ClassInfo>,
) -> bool {
    let mut current = Some(cls_name);
    let mut depth = 0;
    while let Some(name) = current {
        if depth > 10 {
            break;
        }
        depth += 1;
        let Some(cls) = class_defs.get(name) else {
            break;
        };
        if cls.static_fields.contains(field_name) {
            return true;
        }
        if cls.fields.contains_key(field_name) {
            return false;
        }
        current = cls.base_class.as_deref();
    }
    false
}
