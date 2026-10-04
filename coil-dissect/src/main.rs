//! `coil-dissect` — in-memory compile + filtered bytecode / IL / AST dump.

mod dissect;

use std::io::Write;
use std::path::PathBuf;
use std::process::exit;

use compiler::{HostGrants, Pipeline};
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
         \x20 coil-dissect [--log-json | --log-lsp] [--root DIR]... [--entry FILE] <file.hy>\n\
         \x20              [--fn <pat>] [--il] [--il-post] [--hir] [--mir] [--ast] [--expand] [--no-source] [-O LEVEL]\n\
         \x20              [--allow-attach] [--allow-exit] [--allow-exec] [--allow-ffi-exec]\n\
         \x20              [--allow-dload STEM]... [--ffi-search-path DIR]...\n\
         \n\
         Options:\n\
         \x20 --fn <pat>         Filter functions by FQN substring / trailing name\n\
         \x20 --il               Also print pre-opt stack IL\n\
         \x20 --il-post          Also print the optimized IL (before fuse / lowering)\n\
         \x20 --tests            Compile `test` cases too (`__zs_test_N`), like `coil test`\n\
         \x20 --hir              Also print each body's HIR (typed, desugared tree)\n\
         \x20 --mir              Also print the MIR of numeric bodies (dense / LIR)\n\
         \x20 --no-source        Do not interleave source lines in the bytecode\n\
         \x20 -O, --opt-level L  none/0, basic/1, standard/2, aggressive/3, size/s, debug/g\n\
         \x20 --opt-stats        Print IL optimization counters (stderr); --opt-stats-json as JSON\n\
         \x20 --ast              Also print the entry-file AST\n\
         \x20 --expand           Print the entry file after macro expansion (no bytecode)\n\
         \x20 --root DIR         Extra module search directory (repeatable; default `src`)\n\
         \x20 --entry FILE       Entry `.hy` (instead of the positional file); a `.hyc`\n\
         \x20                    archive dumps its bytecode instead of compiling\n\
         \x20 --allow-attach     Allow Stream.attach (default deny)\n\
         \x20 --allow-exit       Allow env::exit (default deny)\n\
         \x20 --allow-exec       Allow env::exec (default deny)\n\
         \x20 --allow-ffi-exec   Allow FFI process-exec symbols (default deny)\n\
         \x20 --allow-dload STEM Allow dload of STEM (repeatable; libc still denied)\n\
         \x20 --ffi-search-path  Extra FFI lookup directory (repeatable; not a grant)\n\
         \x20 --log-json         Emit SARIF 2.1 diagnostics on stdout\n\
         \x20 --log-lsp          Emit LSP Diagnostic NDJSON on stdout\n\
         \x20 -h, --help         Show this help"
    );
}

fn parse_args(args: &[String]) -> Result<(ReportConfig, DissectArgs), String> {
    let mut log_json = false;
    let mut log_lsp = false;
    let mut show_il = false;
    let mut show_ast = false;
    let mut show_expand = false;
    let mut fn_pat: Option<String> = None;
    let mut filename: Option<String> = None;
    let mut extra_roots: Vec<PathBuf> = Vec::new();
    let mut entry_flag: Option<String> = None;
    let mut grants = HostGrants::deny_all();
    let mut show_mir = false;
    let mut show_hir = false;
    let mut show_il_post = false;
    let mut include_tests = false;
    let mut source = true;
    let mut opt_level = compiler::OptLevel::default();
    let mut opt_stats = false;
    let mut opt_stats_json = false;
    let parse_level = |v: &str| {
        compiler::OptLevel::parse(v).map_err(|_| format!("unknown optimization level `{v}`"))
    };
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
            "--mir" => show_mir = true,
            "--hir" => show_hir = true,
            "--il-post" => show_il_post = true,
            "--tests" => include_tests = true,
            "--no-source" => source = false,
            "--opt-stats" => opt_stats = true,
            "--opt-stats-json" => opt_stats_json = true,
            "-O" | "--opt-level" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing LEVEL after -O".to_string())?;
                opt_level = parse_level(v)?;
            }
            s if s.starts_with("--opt-level=") => {
                opt_level = parse_level(s.trim_start_matches("--opt-level="))?;
            }
            s if s.starts_with("-O") && s.len() > 2 => opt_level = parse_level(&s[2..])?,
            "--ast" => show_ast = true,
            "--expand" => show_expand = true,
            "--allow-attach" => grants.allow_attach = true,
            "--allow-exit" => grants.allow_exit = true,
            "--allow-exec" => grants.allow_exec = true,
            "--allow-ffi-exec" => grants.allow_ffi_exec = true,
            "--allow-dload" => {
                i += 1;
                let stem = args
                    .get(i)
                    .ok_or_else(|| "missing STEM after --allow-dload".to_string())?;
                grants.grant_dload_allow(stem.clone());
            }
            s if s.starts_with("--allow-dload=") => {
                grants.grant_dload_allow(s.trim_start_matches("--allow-dload="));
            }
            "--ffi-search-path" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| "missing DIR after --ffi-search-path".to_string())?;
                grants.add_ffi_search_path(PathBuf::from(dir));
            }
            s if s.starts_with("--ffi-search-path=") => {
                grants
                    .add_ffi_search_path(PathBuf::from(s.trim_start_matches("--ffi-search-path=")));
            }
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
            show_expand,
            extra_roots,
            grants,
            show_mir,
            show_hir,
            show_il_post,
            source,
            opt_level,
            opt_stats,
            opt_stats_json,
            include_tests,
        },
    ))
}

fn main() {
    comptime::install();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("coil-dissect".to_string())
            .chain(parts.iter().map(|s| (*s).to_string()))
            .collect()
    }

    #[test]
    fn parse_host_grants_and_search_paths() {
        let (_cfg, args) = parse_args(&argv(&[
            "a.hy",
            "--allow-attach",
            "--allow-exit",
            "--allow-exec",
            "--allow-ffi-exec",
            "--allow-dload",
            "tls",
            "--allow-dload=crypto",
            "--ffi-search-path",
            "./native",
            "--ffi-search-path=./more",
        ]))
        .unwrap();
        assert_eq!(args.filename, "a.hy");
        assert!(args.grants.allow_attach);
        assert!(args.grants.allow_exit);
        assert!(args.grants.allow_exec);
        assert!(args.grants.allow_ffi_exec);
        assert_eq!(
            args.grants.allow_dload,
            vec!["tls".to_string(), "crypto".to_string()]
        );
        assert_eq!(
            args.grants.ffi_search_paths,
            vec![PathBuf::from("./native"), PathBuf::from("./more")]
        );
    }

    #[test]
    fn parse_default_denies_host_grants() {
        let (_cfg, args) = parse_args(&argv(&["a.hy", "--fn", "main", "--il"])).unwrap();
        assert_eq!(args.grants, HostGrants::deny_all());
        assert_eq!(args.fn_pat.as_deref(), Some("main"));
        assert!(args.show_il);
    }

    #[test]
    fn parse_rejects_missing_dload_stem() {
        assert!(parse_args(&argv(&["a.hy", "--allow-dload"])).is_err());
    }
}
