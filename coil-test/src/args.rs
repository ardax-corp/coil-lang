//! `coil-test` argv. Same flags `coil test` has always accepted.

use std::path::PathBuf;

use compiler::{HostGrants, OptLevel};
use reporting::ReportConfig;

use crate::coverage::{CoverageOptions, DEFAULT_LCOV_OUT};
use crate::order::{Order, fresh_seed, parse_seed};
use crate::runner::TestOptions;

/// Default test root when no path is given.
pub const TESTS_DIR: &str = "tests";

/// Environment fallback for `--seed` (CI can pin an order without editing argv).
pub const SEED_ENV: &str = "COIL_TEST_SEED";

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
         \x20 --seed N           Shuffle files and cases with seed N (decimal or 0x hex;\n\
         \x20                    default: random, or $COIL_TEST_SEED; printed in the header)\n\
         \x20 --no-shuffle       Run files in sorted path order and cases in source order\n\
         \x20 -j, --jobs N       Run cases on N reactor workers (default: available CPUs)\n\
         \x20 --show-output      Also print passing cases' output (failures always show it)\n\
         \x20 --coverage         Line coverage of project sources: lcov + summary\n\
         \x20 --coverage-out F   lcov path (default target/coverage/lcov.info; implies --coverage)\n\
         \x20 --coverage-per-test F  Also write test -> file -> lines JSON (implies --coverage)\n\
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
    let mut seed: Option<u64> = None;
    let mut no_shuffle = false;
    let mut jobs: Option<usize> = None;
    let mut show_output = false;
    let mut coverage = false;
    let mut coverage_out: Option<PathBuf> = None;
    let mut per_test_out: Option<PathBuf> = None;
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
            "--no-shuffle" => no_shuffle = true,
            "--show-output" => show_output = true,
            "--coverage" => coverage = true,
            "--coverage-out" => {
                i += 1;
                coverage_out = Some(PathBuf::from(value(i, "FILE", a)?));
            }
            s if s.starts_with("--coverage-out=") => {
                coverage_out = Some(PathBuf::from(s.trim_start_matches("--coverage-out=")));
            }
            "--coverage-per-test" => {
                i += 1;
                per_test_out = Some(PathBuf::from(value(i, "FILE", a)?));
            }
            s if s.starts_with("--coverage-per-test=") => {
                per_test_out = Some(PathBuf::from(s.trim_start_matches("--coverage-per-test=")));
            }
            "-j" | "--jobs" => {
                i += 1;
                jobs = Some(parse_jobs(&value(i, "N", a)?)?);
            }
            s if s.starts_with("--jobs=") => {
                jobs = Some(parse_jobs(s.trim_start_matches("--jobs="))?);
            }
            "--seed" => {
                i += 1;
                seed = Some(parse_seed(&value(i, "N", a)?)?);
            }
            s if s.starts_with("--seed=") => {
                seed = Some(parse_seed(s.trim_start_matches("--seed="))?);
            }
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
    let env_seed = std::env::var(SEED_ENV).ok();
    let order = resolve_order(seed, no_shuffle, env_seed.as_deref(), fresh_seed)?;
    Ok(Parsed::Run(
        config,
        TestOptions {
            root: PathBuf::from(path.unwrap_or_else(|| TESTS_DIR.to_string())),
            fail_fast,
            order,
            jobs: jobs.unwrap_or_else(default_jobs),
            show_output,
            coverage: (coverage || coverage_out.is_some() || per_test_out.is_some()).then(|| {
                CoverageOptions {
                    lcov_out: coverage_out.unwrap_or_else(|| PathBuf::from(DEFAULT_LCOV_OUT)),
                    per_test_out,
                    project_root: std::env::current_dir().unwrap_or_default(),
                }
            }),
            opt_level,
            grants,
            extra_roots,
        },
    ))
}

fn parse_jobs(text: &str) -> Result<usize, String> {
    match text.trim().parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "invalid --jobs `{text}` (expected a count of at least 1)"
        )),
    }
}

/// One reactor worker per available CPU.
fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// `--no-shuffle` > `--seed` > `$COIL_TEST_SEED` > a fresh random seed.
fn resolve_order(
    seed: Option<u64>,
    no_shuffle: bool,
    env_seed: Option<&str>,
    fresh: impl FnOnce() -> u64,
) -> Result<Order, String> {
    if no_shuffle {
        if seed.is_some() {
            return Err("`--seed` and `--no-shuffle` cannot be combined".into());
        }
        return Ok(Order::Sorted);
    }
    if let Some(seed) = seed {
        return Ok(Order::Shuffled(seed));
    }
    match env_seed.map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => parse_seed(text)
            .map(Order::Shuffled)
            .map_err(|e| format!("{SEED_ENV}: {e}")),
        None => Ok(Order::Shuffled(fresh())),
    }
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
    fn seed_and_no_shuffle_flags() {
        let (_, o) = run(&["--seed", "0x2a"]);
        assert_eq!(o.order, Order::Shuffled(42));
        let (_, o) = run(&["--seed=7"]);
        assert_eq!(o.order, Order::Shuffled(7));
        let (_, o) = run(&["--no-shuffle"]);
        assert_eq!(o.order, Order::Sorted);
        assert!(parse_args(&argv(&["--seed", "1", "--no-shuffle"])).is_err());
        assert!(parse_args(&argv(&["--seed", "nope"])).is_err());
        assert!(parse_args(&argv(&["--seed"])).is_err());
    }

    #[test]
    fn jobs_and_show_output_flags() {
        let (_, o) = run(&["-j", "3", "--show-output"]);
        assert_eq!(o.jobs, 3);
        assert!(o.show_output);
        let (_, o) = run(&["--jobs=1"]);
        assert_eq!(o.jobs, 1);
        assert!(!o.show_output);
        let (_, o) = run(&[]);
        assert!(o.jobs >= 1);
        for bad in [&["--jobs", "0"][..], &["-j", "x"], &["--jobs"]] {
            assert!(parse_args(&argv(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn coverage_flags() {
        let (_, o) = run(&[]);
        assert_eq!(o.coverage, None);
        let (_, o) = run(&["--coverage"]);
        assert_eq!(
            o.coverage,
            Some(CoverageOptions {
                lcov_out: PathBuf::from(DEFAULT_LCOV_OUT),
                per_test_out: None,
                project_root: std::env::current_dir().unwrap(),
            })
        );
        let (_, o) = run(&["--coverage-out", "cov.info", "--coverage-per-test=t.json"]);
        assert_eq!(
            o.coverage,
            Some(CoverageOptions {
                lcov_out: PathBuf::from("cov.info"),
                per_test_out: Some(PathBuf::from("t.json")),
                project_root: std::env::current_dir().unwrap(),
            })
        );
        assert!(parse_args(&argv(&["--coverage-out"])).is_err());
    }

    #[test]
    fn order_precedence_flag_env_then_fresh() {
        let fresh = || 99;
        assert_eq!(
            resolve_order(None, false, None, fresh),
            Ok(Order::Shuffled(99))
        );
        assert_eq!(
            resolve_order(None, false, Some("0x10"), fresh),
            Ok(Order::Shuffled(16))
        );
        assert_eq!(
            resolve_order(None, false, Some("  "), fresh),
            Ok(Order::Shuffled(99))
        );
        assert_eq!(
            resolve_order(Some(5), false, Some("0x10"), fresh),
            Ok(Order::Shuffled(5))
        );
        assert_eq!(
            resolve_order(None, true, Some("0x10"), fresh),
            Ok(Order::Sorted)
        );
        assert!(resolve_order(None, false, Some("bad"), fresh).is_err());
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
