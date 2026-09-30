//! `coil dissect` — in-memory compile + filtered bytecode / IL / AST dump.

use std::fs;
use std::path::Path;
use std::process::exit;

use compiler::{
    DissectArtifacts, FnSym, HostGrants, OptLevel, Pipeline, format_bytecode,
    format_bytecode_annotated, format_il, format_symbol_index,
};
use parser::Pratt;
use reporting::{ErrorCode, ReportConfig};

use crate::{fail_and_exit, writer_for};

pub struct DissectArgs {
    pub filename: String,
    pub fn_pat: Option<String>,
    pub show_il: bool,
    pub show_ast: bool,
    /// Print the entry file after macro expansion and exit.
    pub show_expand: bool,
    pub extra_roots: Vec<std::path::PathBuf>,
    pub grants: HostGrants,
    pub show_mir: bool,
    pub show_il_post: bool,
    /// Interleave source lines in the bytecode listing.
    pub source: bool,
    pub opt_level: OptLevel,
    pub opt_stats: bool,
    pub opt_stats_json: bool,
    /// Compile `test("…") { … }` cases too (`__zs_test_N`), like `coil test`.
    pub include_tests: bool,
}

/// Bytecode of a compiled `.hyc` archive (function names from its debug
/// symbols; no IL / MIR / AST).
fn archive_artifacts(path: &str) -> Result<DissectArtifacts, String> {
    let bytes = fs::read(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    let loaded = coil_cli::load_archive_bytes(&bytes)
        .map_err(|_| format!("`{path}` is not a readable bytecode archive"))?;
    let mut functions: Vec<FnSym> = loaded
        .debug
        .fn_symbols
        .iter()
        .map(|sym| FnSym {
            name: sym.name.clone(),
            entry_pc: sym.entry_pc,
            locals: Vec::new(),
            vars: Vec::new(),
        })
        .collect();
    // Archives do not always record function symbols: one section then.
    if functions.is_empty() {
        functions.push(FnSym {
            name: "<program>".into(),
            entry_pc: 0,
            locals: Vec::new(),
            vars: Vec::new(),
        });
    }
    Ok(DissectArtifacts {
        bytecode: loaded.bytecode,
        constants: loaded.constants,
        strings: loaded.strings,
        functions,
        il: None,
        il_post: None,
        debug: loaded.debug,
        classes: Default::default(),
        enums: Default::default(),
    })
}

pub fn cmd_dissect(config: ReportConfig, args: DissectArgs) {
    let format = config.format;
    let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
    pipeline.set_include_tests(args.include_tests);
    pipeline.set_host_grants(args.grants);
    let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    pipeline.bind_project_roots_with_default(dir, args.extra_roots);
    pipeline.set_opt_level(args.opt_level);
    if args.opt_stats || args.opt_stats_json {
        pipeline.set_collect_opt_stats(true);
    }

    let from_archive = args.filename.ends_with(".hyc");
    if from_archive
        && (args.show_il || args.show_il_post || args.show_mir || args.show_ast || args.show_expand)
    {
        fail_and_exit(
            &mut pipeline,
            ErrorCode::InvalidCliFlags,
            "--il / --mir / --ast / --expand need a `.hy` source, not a `.hyc` archive",
        );
    }
    if args.show_expand {
        let text = pipeline.expanded_source(&args.filename);
        let _ = pipeline.finish_reporting();
        match text {
            Some(text) => {
                print!("{text}");
                exit(0);
            }
            None => exit(1),
        }
    }
    if args.show_mir {
        compiler::start_mir_capture();
    }
    let artifacts = if from_archive {
        match archive_artifacts(&args.filename) {
            Ok(a) => a,
            Err(e) => fail_and_exit(&mut pipeline, ErrorCode::MissingInputFile, e),
        }
    } else {
        match pipeline.compile_dissect(&args.filename, args.show_il || args.show_il_post) {
            Ok(a) => a,
            Err(_) => {
                let _ = pipeline.finish_reporting();
                exit(1);
            }
        }
    };
    let mir = if args.show_mir { compiler::take_mir_capture() } else { Vec::new() };
    if args.opt_stats || args.opt_stats_json {
        let stats = compiler::last_opt_stats();
        if args.opt_stats {
            eprint!("{}", stats.format_text());
        }
        if args.opt_stats_json {
            eprintln!("{}", stats.format_json());
        }
    }

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
    let listing = if args.source {
        format_bytecode_annotated(&artifacts, pat)
    } else {
        format_bytecode(&artifacts, pat)
    };
    match listing {
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

    if args.show_il_post {
        println!("=== il (optimized) ===");
        match artifacts.il_post.as_ref().map(|snap| format_il(snap, pat)) {
            Some(Ok(s)) => print!("{s}"),
            Some(Err(e)) => fail_and_exit(&mut pipeline, ErrorCode::InvalidCliFlags, e),
            None => fail_and_exit(
                &mut pipeline,
                ErrorCode::InvalidCliFlags,
                "internal: --il-post requested but no optimized IL snapshot",
            ),
        }
    }

    if args.show_mir {
        println!("=== mir ===");
        let shown: Vec<_> = mir
            .iter()
            .filter(|(name, _, _)| pat.is_none_or(|p| compiler::matches_fn_pat(name, p)))
            .collect();
        if shown.is_empty() {
            println!(";; no numeric body reached MIR (all stay fuse-IL)\n");
        }
        for (name, form, text) in shown {
            // A body may still lose the cost gate to fuse-IL after MIR.
            let kept = artifacts
                .function_ranges()
                .iter()
                .find(|(s, _, _)| &s.name == name)
                .is_some_and(|(_, start, end)| {
                    artifacts.bytecode[*start..*end].iter().any(|b| {
                        let m = b.bytecode().mnemonic();
                        m.starts_with("Dense") || m.starts_with('V')
                    })
                });
            match (*form, kept) {
                ("dense", true) => println!(";; fn {name}  dense MIR (kept)"),
                ("dense", false) => println!(";; fn {name}  dense MIR (not kept: fuse-IL won)"),
                _ => println!(";; fn {name}  {form} MIR"),
            }
            println!("{text}");
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
            Ok((_span, expr)) => print!("{}", parser::format_program(&expr)),
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
