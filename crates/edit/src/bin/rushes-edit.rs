//! `rushes-edit`: the editing tools on the command line, for development and
//! benchmarks. The app serves the same tools from its own binary.
//!
//!   rushes-edit mcp --footage <dir>          serve the tools over MCP on stdio
//!   rushes-edit call <tool> [json] --footage <dir>   run one tool and print the result

use std::path::PathBuf;
use std::process::ExitCode;

use rushes_edit::tools::{Content, Tools};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let footage = match args.iter().position(|arg| arg == "--footage") {
        Some(at) if at + 1 < args.len() => {
            let dir = PathBuf::from(args.remove(at + 1));
            args.remove(at);
            dir
        }
        _ => std::env::current_dir().expect("current dir"),
    };
    match args.first().map(String::as_str) {
        Some("mcp") => {
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout();
            match rushes_edit::mcp::serve(&footage, stdin, stdout) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("rushes-edit: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("call") if args.len() >= 2 => {
            let arguments = match args.get(2) {
                Some(json) => match serde_json::from_str(json) {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("rushes-edit: arguments are not JSON: {error}");
                        return ExitCode::FAILURE;
                    }
                },
                None => serde_json::Value::Null,
            };
            let mut tools = Tools::new(&footage);
            let result = tools.call(&args[1], arguments, &|line| eprintln!("… {line}"));
            for item in &result.content {
                match item {
                    Content::Text(text) => println!("{text}"),
                    Content::Png(bytes) => {
                        let path = std::env::temp_dir().join(format!("rushes-look-{}.png", std::process::id()));
                        let _ = std::fs::write(&path, bytes);
                        println!("[image: {}]", path.display());
                    }
                }
            }
            if result.is_error { ExitCode::FAILURE } else { ExitCode::SUCCESS }
        }
        _ => {
            eprintln!(
                "usage: rushes-edit mcp --footage <dir> | rushes-edit call <tool> [json-arguments] --footage <dir>"
            );
            ExitCode::FAILURE
        }
    }
}
