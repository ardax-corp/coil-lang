//! `coil dissect` — in-memory compile + filtered MIR / HIR / AST dump.

use std::fs;
use std::path::Path;
use std::process::exit;

use compiler::native_mir::NativeBody;
use compiler::{DissectArtifacts, FnSym, HostGrants, OptLevel, Pipeline, format_symbol_index};
use parser::Pratt;
use reporting::{ErrorCode, ReportConfig};

use crate::{fail_and_exit, writer_for};

pub struct DissectArgs {
    pub filename: String,
    pub fn_pat: Option<String>,
    pub show_ast: bool,
    /// Print the entry file after macro expansion and exit.
    pub show_expand: bool,
    pub extra_roots: Vec<std::path::PathBuf>,
    pub grants: HostGrants,
    /// Print each body's HIR (typed, desugared tree).
    pub show_hir: bool,
    pub show_effects: bool,
    /// Print every place lowering boxes a value (`--boxes`).
    pub show_boxes: bool,
    pub opt_level: OptLevel,
    pub contracts: Option<compiler::ContractLevel>,
    /// `--fold-calls on|off`; `None` keeps the pipeline default.
    pub fold_calls: Option<bool>,
    /// `--early-free`; `None` follows the opt level.
    pub early_free: Option<compiler::EarlyFree>,
    /// `--auto-par on|off`; `None` keeps the pipeline default.
    pub auto_par: Option<bool>,
    pub opt_stats: bool,
    pub opt_stats_json: bool,
    /// Compile `test("…") { … }` cases too (`__zs_test_N`), like `coil test`.
    pub include_tests: bool,
}

/// The MIR of a compiled `.ccoil` archive, and its function names.
fn archive_artifacts(path: &str) -> Result<(DissectArtifacts, Vec<NativeBody>), String> {
    let bytes = fs::read(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    let loaded = coil_cli::load_archive_bytes(&bytes).map_err(|_| format!("`{path}` is not a readable archive"))?;
    let functions = loaded
        .debug
        .fn_symbols
        .iter()
        .map(|sym| FnSym { name: sym.name.clone(), body_id: sym.body_id, locals: Vec::new(), vars: Vec::new() })
        .collect();
    let bodies = compiler::native_mir::archive::decode(&loaded.native_mir).map_err(|e| format!("`{path}`: {e}"))?.0;
    let artifacts = DissectArtifacts {
        strings: loaded.strings,
        functions,
        debug: loaded.debug,
        classes: Default::default(),
        enums: Default::default(),
    };
    Ok((artifacts, bodies))
}

pub fn cmd_dissect(config: ReportConfig, args: DissectArgs) {
    let format = config.format;
    let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
    pipeline.set_include_tests(args.include_tests);
    pipeline.set_host_grants(args.grants);
    let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    pipeline.bind_project_roots_with_default(dir, args.extra_roots);
    pipeline.set_opt_level(args.opt_level);
    if let Some(level) = args.contracts {
        pipeline.set_contracts(level);
    }
    if let Some(on) = args.fold_calls {
        pipeline.set_call_fold(on);
    }
    if let Some(mode) = args.early_free {
        pipeline.set_early_free(mode.compiles());
    }
    if let Some(on) = args.auto_par {
        pipeline.set_auto_par(on);
    }
    if args.opt_stats || args.opt_stats_json {
        pipeline.set_collect_opt_stats(true);
    }

    let from_archive = common::ext::is_archive_path(std::path::Path::new(&args.filename));
    if from_archive
        && (args.show_hir || args.show_effects || args.show_ast || args.show_expand)
    {
        fail_and_exit(
            &mut pipeline,
            ErrorCode::InvalidCliFlags,
            "--hir / --effects / --ast / --expand need a `.coil` source, not a `.ccoil` archive",
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
    pipeline.set_runs_mir();
    compiler::native_mir::start_native_capture();
    if args.show_hir {
        compiler::start_hir_capture();
    }
    if args.show_effects {
        compiler::start_effects_capture();
    }
    if args.show_boxes {
        compiler::start_box_capture();
    }
    let (artifacts, bodies) = if from_archive {
        match archive_artifacts(&args.filename) {
            Ok(a) => a,
            Err(e) => fail_and_exit(&mut pipeline, ErrorCode::MissingInputFile, e),
        }
    } else {
        match pipeline.compile_dissect(&args.filename) {
            Ok(a) => (a, compiler::native_mir::take_native_capture()),
            Err(_) => {
                let _ = pipeline.finish_reporting();
                exit(1);
            }
        }
    };
    let hir = if args.show_hir { compiler::take_hir_capture() } else { Default::default() };
    let effects = if args.show_effects { compiler::take_effects_capture() } else { Default::default() };
    let boxes = if args.show_boxes { compiler::take_box_capture() } else { Vec::new() };
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

    if args.show_hir {
        println!("=== hir ===");
        for (_, text) in hir
            .bodies
            .iter()
            .filter(|(name, _)| pat.is_none_or(|p| compiler::matches_fn_pat(name, p)))
        {
            println!("{text}");
        }
        for problem in &hir.problems {
            println!(";; hir problem: {problem}");
        }
    }

    if args.show_effects {
        println!("=== effects ===");
        if effects.fns.is_empty() {
            println!(";; no HIR effects (HIR lowering is off)");
        }
        for (name, text) in effects
            .fns
            .iter()
            .filter(|(name, _)| pat.is_none_or(|p| compiler::matches_fn_pat(name, p)))
        {
            println!("{name}: {text}");
        }
        for (name, text) in effects
            .escape
            .iter()
            .filter(|(name, _)| pat.is_none_or(|p| compiler::matches_fn_pat(name, p)))
        {
            println!(";; escape {name}: {text}");
        }
        if effects.auto_par_off {
            println!(";; auto-par is off");
        }
        for line in &effects.auto_par {
            println!(";; {line}");
        }
        println!();
    }

    if args.show_boxes {
        println!("=== boxes ===");
        let shown: Vec<_> = boxes.iter().filter(|b| pat.is_none_or(|p| compiler::matches_fn_pat(&b.func, p))).collect();
        if shown.is_empty() {
            println!(";; no boxes");
        }
        for b in shown {
            println!("{}: {} `{}` ({})", b.func, b.kind.as_str(), b.ty, b.site);
        }
        println!();
    }

    println!("=== mir ===");
    let shown: Vec<_> = bodies.iter().filter(|b| pat.is_none_or(|p| compiler::matches_fn_pat(&b.name, p))).collect();
    for body in shown {
        println!(";; fn {}", body.name);
        println!("{}", body.mir);
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
