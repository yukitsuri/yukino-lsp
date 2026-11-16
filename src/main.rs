mod call_hierarchy;
mod check;
mod completion;
mod context;
mod definition;
mod diagnostics;
mod doc;
mod document;
mod format;
mod hover;
mod inlay_hints;
mod lint;
mod references;
mod server;
mod signature_help;
mod symbols;
mod type_check;
mod watch;
mod workspace;

use std::env;
use std::fs;
use std::path::Path;
use tower_lsp::{LspService, Server};

use crate::server::YukinoLanguageServer;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str()).unwrap_or("lsp");

    match subcommand {
        "--version" | "-v" | "version" => {
            println!("yukino-lsp {}", env!("CARGO_PKG_VERSION"));
        }
        "lsp" => run_lsp_server().await,
        "check" => {
            let target = args.get(2).map(|s| s.as_str()).unwrap_or(".");
            let json_mode = args.iter().any(|a| a == "--json");
            let code = check::run_check(Path::new(target), json_mode);
            std::process::exit(code);
        }
        "watch" => {
            let target = args.get(2).map(|s| s.as_str()).unwrap_or(".");
            let json_mode = args.iter().any(|a| a == "--json");
            let delay_ms = parse_flag_u64(&args, "--delay").unwrap_or(300);
            watch::run_watch(Path::new(target), delay_ms, json_mode);
        }
        "context" => {
            let target = args.get(2).map(|s| s.as_str()).unwrap_or(".");
            let line = parse_flag_u32(&args, "--line").unwrap_or(0);
            let col = parse_flag_u32(&args, "--col").unwrap_or(0);
            context::run_context(Path::new(target), line, col);
        }
        "doc" => {
            let sym = args.get(2).map(|s| s.as_str()).unwrap_or("");
            let json_mode = args.iter().any(|a| a == "--json");
            doc::run_doc(sym, json_mode);
        }
        "format" => {
            let target = args.get(2).map(|s| s.as_str()).unwrap_or("");
            if target.is_empty() {
                eprintln!("Usage: yukino-lsp format <file.ykn>");
                std::process::exit(1);
            }
            if let Ok(src) = fs::read_to_string(target) {
                let formatted = format::format_source(&src);
                let _ = fs::write(target, formatted);
                println!("ok");
            } else {
                eprintln!("Failed to read {}", target);
                std::process::exit(1);
            }
        }
        "--help" | "-h" | "help" => {
            print_help();
        }
        _ => {
            run_lsp_server().await;
        }
    }
}

async fn run_lsp_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(YukinoLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

fn parse_flag_u64(args: &[String], flag: &str) -> Option<u64> {
    for i in 0..args.len().saturating_sub(1) {
        if args[i] == flag {
            return args[i + 1].parse().ok();
        }
    }
    None
}

fn parse_flag_u32(args: &[String], flag: &str) -> Option<u32> {
    for i in 0..args.len().saturating_sub(1) {
        if args[i] == flag {
            return args[i + 1].parse().ok();
        }
    }
    None
}

fn print_help() {
    println!("yukino-lsp - Language Server & Tooling for the Yukino programming language");
    println!("\nUsage:");
    println!("  yukino-lsp lsp                    Run LSP server over stdio");
    println!("  yukino-lsp check <path> [--json]  Check syntax and emit diagnostics");
    println!("  yukino-lsp watch <path> [--delay <ms>] [--json]  Watch for changes");
    println!("  yukino-lsp context <file> --line <L> --col <C>   Get agentic context");
    println!("  yukino-lsp doc <symbol> [--json]  Show symbol documentation");
    println!("  yukino-lsp format <file.ykn>      Format source file in place");
}
