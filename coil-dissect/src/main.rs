//! `coil-dissect` — in-memory compile + filtered bytecode / IL / AST dump.

mod dissect;

use std::io::Write;
use std::path::PathBuf;
use std::process::exit;

use compiler::Pipeline;
use dissect::{DissectArgs, cmd_dissect};
use reporting::{ErrorCode, ReportConfig, ReportFormat};

fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
    match format {
        ReportFormat::Pretty => Box::new(std::io::stderr()),
        ReportFormat::Sarif | ReportFormat::Lsp => Box::new(std::io::stdout()),
    }
}

fn fail_and_exit(pipeline: &mut Pipeline, code: ErrorCode, message: impl Into<String>) -> ! {
    pipeline.emit_spanless_error(code, message);
    let _ = pipeline.finish_reporting();
    exit(1);
}

fn print_help() {
    eprintln!(
        "Usage:\n\
         \x20 coil-dissect [--log-json | --log-lsp] [--root DIR]... [--entry FILE] <file.hy> [--fn <pat>] [--il] [--ast]\n\
         \n\
         Options:\n\
         \x20 --fn <pat>    Filter functions by FQN substring / trailing name\n\
         \x20 --il          Also print pre-opt stack IL\n\
         \x20 --ast         Also print the entry-file AST\n\
         \x20 --root DIR    Extra module search directory (repeatable; default `src`)\n\
         \x20 --entry FILE  Entry `.hy` (instead of the positional file)\n\
         \x20 --log-json    Emit SARIF 2.1 diagnostics on stdout\n\
         \x20 --log-lsp     Emit LSP Diagnostic NDJSON on stdout\n\
         \x20 -h, --help    Show this help"
    );
}

fn parse_args(args: &[String]) -> Result<(ReportConfig, DissectArgs), String> {
    let mut log_json = false;
    let mut log_lsp = false;
    let mut show_il = false;
    let mut show_ast = false;
    let mut fn_pat: Option<String> = None;
    let mut filename: Option<String> = None;
    let mut extra_roots: Vec<PathBuf> = Vec::new();
    let mut entry_flag: Option<String> = None;
    let mut i = 1usize;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-h" | "--help" => {
                print_help();
                exit(0);
            }
            "--log-json" => log_json = true,
            "--log-lsp" => log_lsp = true,
            "--il" => show_il = true,
            "--ast" => show_ast = true,
            "--fn" => {
                i += 1;
                let pat = args
                    .get(i)
                    .ok_or_else(|| "missing pattern after --fn".to_string())?;
                fn_pat = Some(pat.clone());
            }
            "--root" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| "missing DIR after --root".to_string())?;
                extra_roots.push(PathBuf::from(dir));
            }
            s if s.starts_with("--root=") => {
                extra_roots.push(PathBuf::from(s.trim_start_matches("--root=")));
            }
            "--entry" => {
                i += 1;
                let path = args
                    .get(i)
                    .ok_or_else(|| "missing FILE after --entry".to_string())?;
                entry_flag = Some(path.clone());
            }
            s if s.starts_with("--entry=") => {
                entry_flag = Some(s.trim_start_matches("--entry=").to_string());
            }
            s if s.starts_with('-') => {
                return Err(format!("unrecognized flag `{s}`"));
            }
            _ => {
                if filename.is_some() {
                    return Err("unexpected extra argument".into());
                }
                filename = Some(a.clone());
            }
        }
        i += 1;
    }
    let filename = match (filename, entry_flag) {
        (Some(a), Some(b)) if a != b => {
            return Err("pass the entry as a positional file or `--entry`, not both".into());
        }
        (Some(a), _) => a,
        (_, Some(b)) => b,
        (None, None) => return Err("dissect requires an entry .hy file".into()),
    };
    let config = ReportConfig::from_cli_flags(log_json, log_lsp).map_err(|e| e.to_string())?;
    Ok((
        config,
        DissectArgs {
            filename,
            fn_pat,
            show_il,
            show_ast,
            extra_roots,
        },
    ))
}

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    match parse_args(&raw) {
        Ok((config, args)) => cmd_dissect(config, args),
        Err(msg) => {
            eprintln!("coil-dissect: {msg}");
            print_help();
            exit(1);
        }
    }
}
