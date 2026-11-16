use std::fs;
use std::path::Path;
use tower_lsp::lsp_types::Url;

use crate::diagnostics::analyze_document;
use crate::document::Document;

pub fn run_check(path: &Path, json_mode: bool) -> i32 {
    let explicit_yuki_work = path.components().any(|c| c.as_os_str() == "yuki-work");
    let mut files = Vec::new();
    collect_ykn_files(path, &mut files, explicit_yuki_work);

    let mut total_errors = 0;
    let mut total_warnings = 0;

    for file_path in &files {
        if !explicit_yuki_work && file_path.components().any(|c| c.as_os_str() == "yuki-work") {
            continue;
        }
        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to read {}: {}", file_path.display(), e);
                total_errors += 1;
                continue;
            }
        };

        let abs_path = file_path.canonicalize().unwrap_or_else(|_| {
            std::env::current_dir().map(|cwd| cwd.join(file_path)).unwrap_or_else(|_| file_path.to_path_buf())
        });
        let uri = Url::from_file_path(&abs_path).unwrap_or_else(|_| Url::parse("file:///tmp").unwrap());
        let doc = Document::new(uri, content, 1);
        let env = crate::workspace::WorkspaceEnv::for_path(&abs_path);
        let (_, diags) = analyze_document(&doc, Some(&env));

        for diag in diags {
            if diag.severity == Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR) {
                total_errors += 1;
            } else if diag.severity == Some(tower_lsp::lsp_types::DiagnosticSeverity::WARNING) {
                total_warnings += 1;
            }

            if json_mode {
                let serialized = serde_json::to_string(&diag).unwrap_or_default();
                println!("{serialized}");
            } else {
                let line = diag.range.start.line + 1;
                let col = diag.range.start.character + 1;
                let sev_str = match diag.severity {
                    Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR) => "error",
                    Some(tower_lsp::lsp_types::DiagnosticSeverity::WARNING) => "warning",
                    _ => "info",
                };
                eprintln!("{}:{}:{}: {}: {}", file_path.display(), line, col, sev_str, diag.message);
            }
        }
    }

    if total_errors == 0 && total_warnings == 0 {
        if json_mode {
            println!("{{\"status\":\"ok\",\"errors\":0,\"warnings\":0}}");
        } else {
            println!("ok");
        }
    }

    if total_errors > 0 { 1 } else { 0 }
}

fn collect_ykn_files(path: &Path, files: &mut Vec<std::path::PathBuf>, explicit_yuki_work: bool) {
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "ykn") {
            files.push(path.to_path_buf());
        }
        return;
    }

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name.starts_with('.') || name == "target" || name == "temp" {
                    continue;
                }
                if name == "yuki-work" && !explicit_yuki_work {
                    continue;
                }
                collect_ykn_files(&p, files, explicit_yuki_work);
            } else if p.extension().is_some_and(|ext| ext == "ykn") {
                files.push(p);
            }
        }
    }
}
