//! `coil dissect` — in-memory compile + filtered bytecode / IL / AST dump.

use std::fs;
use std::path::Path;
use std::process::exit;

use compiler::{Pipeline, format_bytecode, format_il, format_symbol_index};
use parser::Pratt;
use reporting::{ErrorCode, ReportConfig};

use crate::{fail_and_exit, writer_for};

pub struct DissectArgs {
    pub filename: String,
    pub fn_pat: Option<String>,
    pub show_il: bool,
    pub show_ast: bool,
    pub extra_roots: Vec<std::path::PathBuf>,
}

pub fn cmd_dissect(config: ReportConfig, args: DissectArgs) {
    let format = config.format;
    let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
    let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    pipeline.bind_project_roots_with_default(dir, args.extra_roots);

    let artifacts = match pipeline.compile_dissect(&args.filename, args.show_il) {
        Ok(a) => a,
        Err(()) => {
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };

    let pat = args.fn_pat.as_deref();

    if let Some(p) = pat {
        let matched: Vec<_> = artifacts
            .functions
            .iter()
            .filter(|s| compiler::matches_fn_pat(&s.name, p))
            .cloned()
            .collect();
        if matched.is_empty() {
            fail_and_exit(
                &mut pipeline,
                ErrorCode::InvalidCliFlags,
                format!("no functions matching `--fn {p}`"),
            );
        }
        print!("{}", format_symbol_index(&matched));
    } else {
        print!("{}", format_symbol_index(&artifacts.functions));
    }
    println!();

    println!("=== bytecode ===");
    match format_bytecode(&artifacts, pat) {
        Ok(s) => print!("{s}"),
        Err(e) => {
            fail_and_exit(&mut pipeline, ErrorCode::InvalidCliFlags, e);
        }
    }

    if args.show_il {
        println!("=== il ===");
        let Some(ref snap) = artifacts.il else {
            fail_and_exit(
                &mut pipeline,
                ErrorCode::InvalidCliFlags,
                "internal: --il requested but no IL snapshot",
            );
        };
        match format_il(snap, pat) {
            Ok(s) => print!("{s}"),
            Err(e) => {
                fail_and_exit(&mut pipeline, ErrorCode::InvalidCliFlags, e);
            }
        }
    }

    if args.show_ast {
        println!("=== ast ===");
        let path = Path::new(&args.filename);
        let src = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                fail_and_exit(
                    &mut pipeline,
                    ErrorCode::MissingInputFile,
                    format!("failed to read {}: {e}", args.filename),
                );
            }
        };
        let parser = Pratt::default();
        match parser.parse(&src) {
            Ok((_span, expr)) => println!("{expr}"),
            Err(err) => {
                fail_and_exit(
                    &mut pipeline,
                    ErrorCode::InvalidCliFlags,
                    format!("parse error in {}: {err:?}", args.filename),
                );
            }
        }
    }

    let _ = pipeline.finish_reporting();
}
