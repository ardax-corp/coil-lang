//! `coil-test` argv. Same flags `coil test` has always accepted.

use std::path::PathBuf;

use compiler::{HostGrants, OptLevel};
use reporting::ReportConfig;

use crate::runner::TestOptions;

/// Default test root when no path is given.
pub const TESTS_DIR: &str = "tests";

pub enum Parsed {
    Help,
    Run(ReportConfig, TestOptions),
}

pub fn print_help() {
    eprintln!(
        "Compile and run every test under [PATH] (default: ./tests)\n\
         \n\
         Usage:\n\
         \x20 coil test [OPTIONS] [PATH]\n\
         \n\
         Files under a `compile_fail/` directory must be rejected by the compiler.\n\
         \n\
         Options:\n\
         \x20 --fail-fast        Stop after the first failed case\n\
         \x20 -O, --opt-level L  none/0, basic/1, standard/2 (default), aggressive/3, size/s, debug/g\n\
         \x20 --root DIR         Extra module search directory (repeatable; default `src`)\n\
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

/// Parse argv (including argv0).
pub fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut log_json = false;
    let mut log_lsp = false;
    let mut fail_fast = false;
    let mut path: Option<String> = None;
    let mut extra_roots: Vec<PathBuf> = Vec::new();
    let mut grants = HostGrants::deny_all();
    let mut opt_level = OptLevel::default();
    let parse_level = |v: &str| {
        OptLevel::parse(v).map_err(|_| {
            format!(
                "invalid --opt-level `{v}` (expected none|basic|standard|aggressive|size|debug or 0|1|2|3|s|g)"
            )
        })
    };
    let value = |i: usize, what: &str, flag: &str| -> Result<String, String> {
        args.get(i)
            .cloned()
            .ok_or_else(|| format!("missing {what} after {flag}"))
    };

    let mut i = 1usize;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--log-json" => log_json = true,
            "--log-lsp" => log_lsp = true,
            "--fail-fast" => fail_fast = true,
            "-O" | "--opt-level" => {
                i += 1;
                opt_level = parse_level(&value(i, "LEVEL", a)?)?;
            }
            s if s.starts_with("--opt-level=") => {
                opt_level = parse_level(s.trim_start_matches("--opt-level="))?;
            }
            s if s.starts_with("-O") && s.len() > 2 => opt_level = parse_level(&s[2..])?,
            "--allow-attach" => grants.allow_attach = true,
            "--allow-exit" => grants.allow_exit = true,
            "--allow-exec" => grants.allow_exec = true,
            "--allow-ffi-exec" => grants.allow_ffi_exec = true,
            "--allow-dload" => {
                i += 1;
                grants.grant_dload_allow(value(i, "STEM", a)?);
            }
            s if s.starts_with("--allow-dload=") => {
                grants.grant_dload_allow(s.trim_start_matches("--allow-dload="));
            }
            "--ffi-search-path" => {
                i += 1;
                grants.add_ffi_search_path(PathBuf::from(value(i, "DIR", a)?));
            }
            s if s.starts_with("--ffi-search-path=") => {
                grants
                    .add_ffi_search_path(PathBuf::from(s.trim_start_matches("--ffi-search-path=")));
            }
            "--root" => {
                i += 1;
                extra_roots.push(PathBuf::from(value(i, "DIR", a)?));
            }
            s if s.starts_with("--root=") => {
                extra_roots.push(PathBuf::from(s.trim_start_matches("--root=")));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("unrecognized flag `{s}`"));
            }
            _ => {
                if path.is_some() {
                    return Err(format!("unexpected extra argument `{a}`"));
                }
                path = Some(a.to_string());
            }
        }
        i += 1;
    }

    let config = ReportConfig::from_cli_flags(log_json, log_lsp).map_err(|e| e.to_string())?;
    Ok(Parsed::Run(
        config,
        TestOptions {
            root: PathBuf::from(path.unwrap_or_else(|| TESTS_DIR.to_string())),
            fail_fast,
            opt_level,
            grants,
            extra_roots,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("coil-test".to_string())
            .chain(parts.iter().map(|s| (*s).to_string()))
            .collect()
    }

    fn run(parts: &[&str]) -> (ReportConfig, TestOptions) {
        match parse_args(&argv(parts)).expect("parses") {
            Parsed::Run(config, options) => (config, options),
            Parsed::Help => panic!("unexpected help"),
        }
    }

    #[test]
    fn defaults_to_tests_dir_standard_opt_and_deny_all() {
        let (_, o) = run(&[]);
        assert_eq!(o.root, PathBuf::from(TESTS_DIR));
        assert!(!o.fail_fast);
        assert_eq!(o.opt_level, OptLevel::Standard);
        assert_eq!(o.grants, HostGrants::deny_all());
        assert!(o.extra_roots.is_empty());
    }

    #[test]
    fn path_fail_fast_opt_roots_and_grants() {
        let (_, o) = run(&[
            "./suite",
            "--fail-fast",
            "-O0",
            "--root",
            "examples/src",
            "--root=lib",
            "--allow-exit",
            "--allow-dload",
            "plugin",
            "--ffi-search-path=native",
        ]);
        assert_eq!(o.root, PathBuf::from("./suite"));
        assert!(o.fail_fast);
        assert_eq!(o.opt_level, OptLevel::None);
        assert_eq!(
            o.extra_roots,
            vec![PathBuf::from("examples/src"), PathBuf::from("lib")]
        );
        assert!(o.grants.allow_exit && !o.grants.allow_exec);
        assert_eq!(o.grants.allow_dload, vec!["plugin".to_string()]);
        assert_eq!(o.grants.ffi_search_paths, vec![PathBuf::from("native")]);

        let (_, o) = run(&["--opt-level", "aggressive"]);
        assert_eq!(o.opt_level, OptLevel::Aggressive);
        let (_, o) = run(&["--opt-level=g"]);
        assert_eq!(o.opt_level, OptLevel::Debug);
    }

    #[test]
    fn log_flags_select_report_format() {
        let (c, _) = run(&["--log-lsp"]);
        assert_eq!(c.format, reporting::ReportFormat::Lsp);
        let (c, _) = run(&[]);
        assert_eq!(c.format, reporting::ReportFormat::Pretty);
    }

    #[test]
    fn rejects_unknown_flags_extra_paths_and_missing_values() {
        for bad in [
            &["-o", "x"][..],
            &["--include-tests"],
            &["--opt-stats"],
            &["a", "b"],
            &["--root"],
            &["-O", "fast"],
            &["--allow-dload"],
        ] {
            assert!(parse_args(&argv(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn help_wins() {
        assert!(matches!(
            parse_args(&argv(&["--fail-fast", "--help"])),
            Ok(Parsed::Help)
        ));
        assert!(matches!(parse_args(&argv(&["-h"])), Ok(Parsed::Help)));
    }
}
