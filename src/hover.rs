use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};
use yukic_ast::{ItemKind, Module};

use crate::document::Document;
use crate::workspace::WorkspaceEnv;

pub fn compute_hover(
    doc: &Document,
    pos: Position,
    module: &Module,
    env: &WorkspaceEnv,
) -> Option<Hover> {
    let offset = doc.position_to_byte_offset(pos);
    let current_file = doc.uri.to_file_path().unwrap_or_default();

    // Hovering on a `cload "header.h"` declaration shows resolved path & imported symbols.
    for item in &module.items {
        if let ItemKind::Cload(ref cl) = item.kind {
            if offset >= item.span.start.0 as usize && offset <= item.span.end.0 as usize {
                let hdr = env.get_or_load_cheader(&cl.path, &current_file);
                if let Some(ref err) = hdr.error {
                    return Some(create_markdown_hover(&format!(
                        "**C Header:** `{}`\n\nError: {err}",
                        cl.path
                    )));
                }
                let preview: Vec<String> = hdr
                    .exports
                    .iter()
                    .take(15)
                    .map(|e| format!("- `{}`", e.signature))
                    .collect();
                let more = if hdr.exports.len() > 15 {
                    format!("\n... and {} more symbols", hdr.exports.len() - 15)
                } else {
                    String::new()
                };
                return Some(create_markdown_hover(&format!(
                    "**C Header:** `{}` (`{}::`)\nResolved: `{}` ({} symbols)\n\n{}{}",
                    cl.path,
                    hdr.namespace,
                    hdr.header_path.display(),
                    hdr.exports.len(),
                    preview.join("\n"),
                    more
                )));
            }
        }
    }

    let word = get_word_at_offset(&doc.text, offset)?;

    if let Some(doc_str) = get_ast_symbol_doc(word, module) {
        return Some(create_markdown_hover(&doc_str));
    }

    for (_decl, hdr) in env.collect_cload_headers(module, &current_file) {
        if word == hdr.namespace {
            return Some(create_markdown_hover(&format!(
                "**C Header Namespace** `{}` (from `{}`)\n\n{} symbols exported.",
                hdr.namespace,
                hdr.rel_path,
                hdr.exports.len()
            )));
        }
        if let Some(exp) = hdr.exports.iter().find(|e| e.name == word) {
            return Some(create_markdown_hover(&format!(
                "```yukino\n{}\n```\nLoaded from `{}` (`{}::{}`).",
                exp.signature, hdr.rel_path, hdr.namespace, exp.name
            )));
        }
    }

    if let Some(doc_str) = get_imported_symbol_doc(word, module, &current_file, env) {
        return Some(create_markdown_hover(&doc_str));
    }

    if let Some(doc_str) = get_keyword_doc(word) {
        return Some(create_markdown_hover(doc_str));
    }

    None
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

fn create_markdown_hover(content: &str) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: content.to_string(),
        }),
        range: None,
    }
}

fn get_ast_symbol_doc(word: &str, module: &Module) -> Option<String> {
    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) if f.name.name == word => {
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
                return Some(format!(
                    "```yukino\nfun {}({}){}\n```\nDeclared in current module.",
                    f.name.name,
                    params.join(", "),
                    ret
                ));
            }
            ItemKind::Cfun(ref c) if c.name.name == word => {
                let mut params: Vec<String> = c
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name.name, p.ty))
                    .collect();
                if c.is_variadic {
                    params.push("...".to_string());
                }
                let ret = if let Some(ref rt) = c.return_type {
                    format!(" -> {rt}")
                } else {
                    " -> void".to_string()
                };
                return Some(format!(
                    "```yukino\ncfun {}({}){};\n```\nC ABI function declared in current module.",
                    c.name.name,
                    params.join(", "),
                    ret
                ));
            }
            ItemKind::Cvar(ref cv) if cv.name.name == word => {
                return Some(format!(
                    "```yukino\ncvar {}: {};\n```\nExternal C global variable declared in current module.",
                    cv.name.name, cv.ty
                ));
            }
            ItemKind::Struct(ref s) if s.name.name == word => {
                let fields: Vec<String> = s
                    .fields
                    .iter()
                    .map(|f| format!("    {}: {}", f.name.name, f.ty))
                    .collect();
                let body = if fields.is_empty() {
                    format!("struct {} {{}}", s.name.name)
                } else {
                    format!("struct {} {{\n{}\n}}", s.name.name, fields.join(",\n"))
                };
                return Some(format!(
                    "```yukino\n{body}\n```\nValue-type struct declared in current module."
                ));
            }
            ItemKind::Cstruct(ref cs) if cs.name.name == word => {
                let kw = if cs.is_union { "cunion" } else { "cstruct" };
                let fields: Vec<String> = cs
                    .fields
                    .iter()
                    .map(|f| format!("    {}: {}", f.name.name, f.ty))
                    .collect();
                let body = if fields.is_empty() {
                    format!("{kw} {} {{}}", cs.name.name)
                } else {
                    format!("{kw} {} {{\n{}\n}}", cs.name.name, fields.join(",\n"))
                };
                return Some(format!(
                    "```yukino\n{body}\n```\nC-compatible {kw} declared in current module."
                ));
            }
            ItemKind::Class(ref c) if c.name.name == word => {
                let prefix = if c.is_locked { "locked class" } else { "class" };
                return Some(format!(
                    "```yukino\n{prefix} {}\n```\nReference-type class declared in current module.",
                    c.name.name
                ));
            }
            ItemKind::Enum(ref e) if e.name.name == word => {
                let variants: Vec<String> = e
                    .variants
                    .iter()
                    .map(|v| match v {
                        yukic_ast::EnumVariant::Simple(id) => format!("    {}", id.name),
                        yukic_ast::EnumVariant::WithValue(id, _) => format!("    {} = ...", id.name),
                        yukic_ast::EnumVariant::Adt(id, fields) => {
                            let fld_strs: Vec<String> = fields
                                .iter()
                                .map(|f| format!("{}: {}", f.name.name, f.ty))
                                .collect();
                            format!("    {}({})", id.name, fld_strs.join(", "))
                        }
                    })
                    .collect();
                let body = if variants.is_empty() {
                    format!("enum {} {{}}", e.name.name)
                } else {
                    format!("enum {} {{\n{}\n}}", e.name.name, variants.join(",\n"))
                };
                return Some(format!(
                    "```yukino\n{body}\n```\nEnumeration declared in current module."
                ));
            }
            ItemKind::Interface(ref i) if i.name.name == word => {
                return Some(format!(
                    "```yukino\ninterface {}\n```\nInterface contract declared in current module.",
                    i.name.name
                ));
            }
            ItemKind::Static(ref s) if s.name.name == word => {
                let ty_str = if let Some(ref ty) = s.ty {
                    format!(": {ty}")
                } else {
                    String::new()
                };
                return Some(format!(
                    "```yukino\nstatic {}{}\n```\nImmutable constant declared in current module.",
                    s.name.name, ty_str
                ));
            }
            ItemKind::Macro(ref m) if m.name.name == word => {
                return Some(format!(
                    "```yukino\ndefine {}!(...)\n```\nMacro declared in current module.",
                    m.name.name
                ));
            }
            ItemKind::TypeAlias(ref ta) if ta.name.name == word => {
                return Some(format!(
                    "```yukino\ntype {} = {}\n```\nType alias declared in current module.",
                    ta.name.name, ta.target
                ));
            }
            ItemKind::Alias(ref a) if a.alias.name == word => {
                return Some(format!(
                    "```yukino\nalias as {}\n```\nAlias declared in current module.",
                    a.alias.name
                ));
            }
            ItemKind::Externc(ref ext) => {
                for ext_item in &ext.items {
                    match ext_item {
                        yukic_ast::ExterncItem::Cfun(ref c) if c.name.name == word => {
                            let mut params: Vec<String> = c
                                .params
                                .iter()
                                .map(|p| format!("{}: {}", p.name.name, p.ty))
                                .collect();
                            if c.is_variadic {
                                params.push("...".to_string());
                            }
                            let ret = if let Some(ref rt) = c.return_type {
                                format!(" -> {rt}")
                            } else {
                                " -> void".to_string()
                            };
                            return Some(format!(
                                "```yukino\ncfun {}({}){};\n```\nExternal C function inside externc block.",
                                c.name.name,
                                params.join(", "),
                                ret
                            ));
                        }
                        yukic_ast::ExterncItem::Cstruct(ref cs) if cs.name.name == word => {
                            let kw = if cs.is_union { "cunion" } else { "cstruct" };
                            let fields: Vec<String> = cs
                                .fields
                                .iter()
                                .map(|f| format!("    {}: {}", f.name.name, f.ty))
                                .collect();
                            let body = if fields.is_empty() {
                                format!("{kw} {} {{}}", cs.name.name)
                            } else {
                                format!("{kw} {} {{\n{}\n}}", cs.name.name, fields.join(",\n"))
                            };
                            return Some(format!(
                                "```yukino\n{body}\n```\nExternal C aggregate inside externc block."
                            ));
                        }
                        yukic_ast::ExterncItem::Cvar(ref cv) if cv.name.name == word => {
                            return Some(format!(
                                "```yukino\ncvar {}: {};\n```\nExternal C global variable inside externc block.",
                                cv.name.name, cv.ty
                            ));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn get_imported_symbol_doc(
    word: &str,
    module: &Module,
    current_file: &std::path::Path,
    env: &WorkspaceEnv,
) -> Option<String> {
    for item in &module.items {
        if let ItemKind::Import(ref imp) = item.kind {
            if let Some(target_file) = env.resolve_import(&imp.path, current_file) {
                if let Some(cached) = env.get_or_parse_module(&target_file) {
                    for export in &cached.exports {
                        if export.name == word {
                            return Some(format!(
                                "```yukino\n{}\n```\nImported from `{}`.",
                                export.signature,
                                target_file.display()
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn get_keyword_doc(word: &str) -> Option<&'static str> {
    match word {
        // Built-in Codegen & Optimization Attributes
        "inline" => Some(
            "```yukino\n#[inline]\nfun fast_add(a: int32, b: int32) -> int32\n```\n**Compiler Attribute:** Gives the LLVM optimizer a strong recommendation (`inlinehint`) to inline the function body at call sites.",
        ),
        "inline_always" => Some(
            "```yukino\n#[inline_always]\nfun bit_get(mask: uint32, index: uint32) -> bool\n```\n**Compiler Attribute:** Forces the function body to be inlined at any optimization level (`alwaysinline`).",
        ),
        "inline_never" | "no_inline" => Some(
            "```yukino\n#[no_inline]\nfun heavy_diagnostic_report()\n```\n**Compiler Attribute:** Strictly forbids inlining the function (`noinline`), preventing instruction cache bloat.",
        ),
        "cold" => Some(
            "```yukino\n#[cold]\nfun slow_error_handler(code: int32)\n```\n**Compiler Attribute:** Marks a function or branch as unlikely to execute (`cold`), moving it out of hot CPU cache loops.",
        ),
        "noreturn" => Some(
            "```yukino\n#[noreturn]\nfun panic_handler(msg: string) -> never\n```\n**Compiler Attribute:** Informs the optimizer and type checker that the function never returns control to the caller.",
        ),
        "stable" => Some(
            "```yukino\n#[stable]\nfun square(x: int64) -> int64\n```\n**Compiler Attribute:** Pure function attribute. Marks function as deterministic, terminating, and side-effect-free (`nounwind`, `willreturn`, `readnone`), enabling aggressive compile-time evaluation and CSE.",
        ),
        "fast_math" => Some(
            "```yukino\n#[fast_math]\npublic fun compute_fma(a: float64, b: float64, c: float64) -> float64\n```\n**Compiler Attribute:** Enables aggressive IEEE-754 FastMath optimizations (FMA instruction generation, vectorization, reassociation). Can also be applied at file scope (`#![fast_math]`).",
        ),
        "no_region" => Some(
            "```yukino\n#[no_region]\npublic fun scale_elements(ref ~arr: float32[], factor: float32)\n```\n**Compiler Attribute:** Eliminates the function's regional memory prologue (`yukino_region_init`) and epilogue (`yukino_region_destroy`) for zero-overhead in-place numeric mutations.",
        ),
        "thunk" => Some(
            "```yukino\n#[thunk]\nfun thread_worker()\n```\n**Compiler Attribute:** Generates standard C ABI flat calling convention without hidden return destination region parameters. Enables passing ordinary Yukino functions as C-compatible callbacks or thread routines in `std.threads: spawn`.",
        ),
        "mangle" => Some(
            "```yukino\n#[mangle]\ncfun mangled_c(val: int32) -> int32\n```\n**Compiler Attribute:** Forcibly enables Yukino name mangling for a C ABI function.",
        ),

        // Built-in Testing Attributes
        "test" => Some(
            "```yukino\n#[test]\nfun test_addition() {\n    assert_eq(2 + 2, 4);\n}\n```\n**Testing Attribute:** Marks a top-level zero-argument function as a unit test for the `yukino test` harness.",
        ),
        "should_panic" => Some(
            "```yukino\n#[test, should_panic]\n#[test, should_panic = \"invalid handle\"]\nfun test_failure()\n```\n**Testing Attribute:** Expects the test to throw a panic. Can optionally specify an expected panic message substring.",
        ),
        "ignore" => Some(
            "```yukino\n#[test, ignore]\n#[test, ignore = \"requires docker\"]\nfun test_integration()\n```\n**Testing Attribute:** Temporarily skips/disables the test during regular `yukino test` runs.",
        ),
        "timeout_ms" => Some(
            "```yukino\n#[test, timeout_ms = 500]\nfun test_fast_io()\n```\n**Testing Attribute:** Sets the maximum unit test execution timeout in milliseconds. Worker process is killed if timeout is exceeded.",
        ),

        // Conditional Compilation Attributes
        "cfg" => Some(
            "```yukino\n#[cfg(all(target_os = \"linux\", feature = \"ssl\"))]\nfun init_fast_ssl()\n```\n**Conditional Compilation:** Prunes inactive AST subtrees before name resolution. Supports `all(...)`, `any(...)`, `not(...)`, with predicates `target_os`, `target_arch`, `target_env`, and `feature`.",
        ),
        "cfg_attr" => Some(
            "```yukino\n#[cfg_attr(target_os = \"linux\", inline_always)]\nfun low_level_call()\n```\n**Conditional Compilation:** Dynamically attaches target attribute only when condition evaluates to true.",
        ),
        "target" => Some(
            "```yukino\n#[target = \"x86_64-unknown-linux-gnu\"]\n#[!target = \"x86_64-pc-windows-gnu\"]\nfun platform_specific()\n```\n**Conditional Compilation:** Shorthand filter by target platform triple. Supports `!` prefix negation.",
        ),
        "arch" => Some(
            "```yukino\n#[arch = \"x86_64\"]\n#[arch = \"arm64\"]\nfun simd_routine()\n```\n**Conditional Compilation:** Shorthand filter by CPU architecture (`\"x86_64\"` or `\"arm64\"`). Supports `!` prefix negation.",
        ),
        "feature" => Some(
            "```yukino\n#[feature = \"ssl\"]\n#[!feature = \"ssl\"]\nfun secure_connect()\n```\n**Conditional Compilation:** Shorthand filter by enabled build feature from `yuki.toml` manifest.",
        ),
        "deprecated" => Some(
            "```yukino\n#[deprecated]\n#[deprecated = \"Use new_engine() instead\"]\nfun legacy_engine()\n```\n**Linter Attribute:** Marks API as deprecated. Emits compiler warning `W0401` when referenced.",
        ),
        "mutation" => Some(
            "```yukino\n#[mutation(JsonSerialize)]\nstruct ConfigPayload\n```\n**Type Mutation:** Attaches procedural code generator companion logic to types.",
        ),

        // C ABI Attributes (|...|)
        "cdecl" => Some(
            "```yukino\n|cdecl|\ncfun puts(s: ptr<c_char>) -> c_int;\n```\n**C ABI Attribute:** Platform standard C calling convention (caller cleans arguments from stack).",
        ),
        "stdcall" => Some(
            "```yukino\n|stdcall|\ncfun GetSystemMetrics(index: int32) -> int32;\n```\n**C ABI Attribute:** Win32 API standard call convention (callee cleans stack).",
        ),
        "fastcall" => Some(
            "```yukino\n|fastcall|\ncfun process_chunk(src: ptr<void>, len: int64) -> int32;\n```\n**C ABI Attribute:** Fastcall calling convention passing first parameters via CPU registers.",
        ),
        "sysv64" => Some(
            "```yukino\n|sysv64|\ncfun posix_routine() -> int32;\n```\n**C ABI Attribute:** Explicit System V AMD64 ABI convention for POSIX/Linux systems.",
        ),
        "win64" => Some(
            "```yukino\n|win64|\ncfun win64_routine() -> int32;\n```\n**C ABI Attribute:** Explicit Microsoft x64 calling convention.",
        ),
        "packed" => Some(
            "```yukino\n|packed|\ncstruct EthernetHeader {\n    dst_mac: uint64,\n    src_mac: uint64,\n    ethertype: uint16,\n}\n```\n**C ABI Attribute:** Disables all padding bytes between cstruct fields for network packets and hardware buffers.",
        ),
        "align" => Some(
            "```yukino\n|align=\"16\"|\ncstruct AlignedBuffer {\n    data: uint32,\n}\n```\n**C ABI Attribute:** Sets explicit machine byte alignment for cstruct (e.g. for SSE/AVX vectors or DMA).",
        ),
        "weak" => Some(
            "```yukino\n|weak, cdecl|\ncfun optional_plugin_hook() -> int32;\n```\n**C ABI Attribute:** Weak symbol linkage (symbol can be overridden or omitted at link time).",
        ),
        "hot" => Some(
            "```yukino\n|hot|\ncfun critical_kernel() -> int32;\n```\n**C ABI Attribute:** Speed-critical function with prioritized inlining and code cache placement.",
        ),
        "returns_nonnull" => Some(
            "```yukino\n|returns_nonnull, cdecl|\ncfun custom_malloc(size: int64) -> ptr<void>;\n```\n**C ABI Attribute:** Guarantees to LLVM that the returned pointer is never NULL.",
        ),
        "section" => Some(
            "```yukino\n|section=\"hot_code\"|\ncfun fast_worker(data: int32) -> int32;\n```\n**C ABI Attribute:** Places function in specialized ELF/PE binary section.",
        ),

        // Language Keywords & Constructs
        "set" => Some(
            "```yukino\nset x: int32 = 10\nset name = \"Yukino\"\n```\nDeclares a local variable binding.",
        ),
        "static" => Some(
            "```yukino\nstatic MAX_USERS: int32 = 100\n```\nDeclares a compile-time constant or immutable module-level variable.",
        ),
        "cstruct" => Some(
            "```yukino\n|packed, align=\"4\"|\ncstruct PacketHeader {\n    magic: uint16,\n    length: uint32,\n}\n```\nDeclares a C-compatible structure following target platform ABI layout and field alignments.",
        ),
        "cunion" => Some(
            "```yukino\ncunion Reg {\n    as_int: c_int,\n    as_float: float32,\n}\n```\nDeclares a C-compatible union sharing storage across fields (initialize 0 or 1 field).",
        ),
        "cvar" => Some(
            "```yukino\ncvar global_errno: c_int;\n```\nDeclares an external C global variable.",
        ),
        "cload" => Some(
            "```yukino\ncload \"include/api.h\"\n```\nLoads a C `.h` header relative to the project manifest directory and makes symbols available directly and via `<header_stem>::symbol`.",
        ),
        "externc" => Some(
            "```yukino\nexternc \"math.h\" {\n    cfun sin(x: float64) -> float64;\n    cfun cos(x: float64) -> float64;\n}\n```\nGroups external C function (`cfun`) and structure (`cstruct`) declarations.",
        ),
        "cfun" => Some(
            "```yukino\n|cdecl|\ncfun puts(s: ptr<c_char>) -> c_int;\n```\nDeclares an external C ABI function without name mangling.",
        ),
        "null" => Some(
            "```yukino\nset user: User? = null\n```\nNull literal representing absence of value. Non-nullable types in Yukino cannot be null; only optional types `T?` can hold null.",
        ),
        "space" => Some(
            "```yukino\nfun on_tick() {\n    space\n}\n```\nExplicit no-op statement suppressing empty block/function compiler warnings.",
        ),
        "this" => Some(
            "```yukino\nrewrite this.name = name\n```\nRefers to the current class instance within its methods and constructors.",
        ),
        "super" => Some(
            "```yukino\nsuper(name)\n```\nRefers to the base class constructor or base class methods in single inheritance.",
        ),
        "init" => Some(
            "```yukino\nset dog = init Dog(\"Barsik\", \"Husky\")\n```\nInvokes or defines class constructors.",
        ),
        "ref" => Some(
            "```yukino\nfun process(ref ~buf: uint8[], ref label: string)\n```\nBorrows a value without moving ownership: `ref` is shared immutable, `ref ~` is exclusive mutable.",
        ),
        "rewrite" => Some(
            "```yukino\nrewrite balance = 200\nrewrite balance += 50\n```\nYukino strictly forbids implicit assignment; all state mutations must start with `rewrite`.",
        ),
        "region" => Some(
            "```yukino\nregion {\n    // temporary allocations\n}\n```\nDefines a local memory region. All allocations within the region are freed collectively in O(1) on block exit.",
        ),
        "defer" => Some(
            "```yukino\ndefer {\n    cleanup_resources()\n}\n```\nDefers statement or block execution until the enclosing scope exits, executed in strict LIFO order.",
        ),
        "sus" => Some(
            "```yukino\nsus {\n    do_risky_io()\n} catch e: IoError {\n    println(e.message)\n} finally {\n    cleanup()\n}\n```\nYukino error handling block with strongly-typed catch clauses and zero happy-path overhead.",
        ),
        "catch" => Some(
            "```yukino\ncatch e: IoError {\n    println(e.message)\n}\n```\nCatches runtime panics matching the specified typed error structure.",
        ),
        "finally" => Some(
            "```yukino\nfinally {\n    close_handle()\n}\n```\nGuaranteed cleanup block that executes after `sus` succeeds or after a `catch` block.",
        ),
        "panic" => Some(
            "```yukino\npanic(IoError { message: \"failed\", code: 101 })\n```\nGenerates a strongly-typed runtime exception. Raw strings or primitives cannot be passed to panic.",
        ),
        "locked" => Some(
            "```yukino\nlocked class GuardService {\n    active: bool\n}\n```\nForbids subclassing / inheritance from this class.",
        ),
        "operator" => Some(
            "```yukino\noperator +(other: Complex) -> Complex {\n    return init Complex(this.re + other.re, this.im + other.im)\n}\n```\nOverloads an operator for a class.",
        ),
        "define" => Some(
            "```yukino\ndefine square(x) => x * x\n```\nDeclares a compile-time syntactic macro.",
        ),
        "alias" => Some(
            "```yukino\nalias std::io as io\nalias std::io#print as println\n```\nCreates a local namespace or symbol alias.",
        ),
        "load" => Some(
            "```yukino\nload std.io: println\npublic load .submod: *\n```\nImports modules, packages, or re-exports symbols.",
        ),
        "as" => Some(
            "```yukino\nload root.math: add as my_add\n```\nRenames imported symbols or specifies alias targets.",
        ),
        "is" => Some(
            "```yukino\nif val is null { ... }\nif payload is int32 { ... }\n```\nType membership and null checking operator.",
        ),
        "with" => Some(
            "```yukino\nclass Dog with Describable { ... }\n```\nSpecifies implemented interfaces for a class.",
        ),
        "extends" => Some(
            "```yukino\nclass Dog extends Animal { ... }\n```\nSpecifies base class in single inheritance.",
        ),
        "recur" => Some(
            "```yukino\nrecur(n - 1, acc * n)\n```\nExplicit tail-call recursion optimization.",
        ),
        "loop" => Some(
            "```yukino\nloop {\n    if done { break }\n}\n```\nUnconditional infinite loop with zero entry condition check overhead.",
        ),
        "cast" => Some(
            "```yukino\nset small: int32 = cast(count, \"int32\")\n```\nExplicit type conversion intrinsic. Implicit type narrowing or widening is prohibited.",
        ),
        "typeof" => Some(
            "```yukino\nset same_type: typeof!(expr) = ...\n```\nCompile-time type introspection macro.",
        ),
        _ => None,
    }
}
