//! `coil-test` argv. Same flags `coil test` has always accepted.

use std::path::PathBuf;

use clap::{Command, CommandFactory, Parser};
use coil_args::{
    HostGrantFlags, LogFlags, OptLevelFlags, RootFlags, expand_o_shorts, parse_with,
    print_command_help,
};
use reporting::ReportConfig;

use crate::coverage::{CoverageOptions, DEFAULT_LCOV_OUT};
use crate::mutate::MutateOptions;
use crate::mutate::job::Isolation;
use crate::mutate::sites::Operator;
use crate::order::{Order, fresh_seed, parse_seed};
use crate::runner::{Report, TestOptions};

/// Default test root when no path is given.
pub const TESTS_DIR: &str = "tests";

/// Environment fallback for `--seed` (CI can pin an order without editing argv).
pub const SEED_ENV: &str = "COIL_TEST_SEED";

pub enum Parsed {
    Help,
    Run(ReportConfig, Box<TestOptions>),
    MutateHelp,
    Mutate(ReportConfig, Box<MutateOptions>),
}

/// First argument that selects mutation testing (`coil mutate` passes it).
pub const MUTATE: &str = "mutate";

/// Default `--timeout-factor`.
pub const DEFAULT_TIMEOUT_FACTOR: u64 = 10;

/// Default `--wall-timeout` (seconds per mutant).
pub const DEFAULT_WALL_TIMEOUT: u64 = 60;

#[derive(Parser, Debug)]
#[command(
    name = "coil-test",
    about = "Compile and run every test under [PATH] (default: ./tests)",
    disable_help_subcommand = true,
    after_help = "Files under a `compile_fail/` directory must be rejected by the compiler\n\
with an error code their header declares (`// Expected: E0209 — why`).\n\
\n\
`--seed N` shuffles files and cases (decimal or 0x hex; default: random,\n\
or $COIL_TEST_SEED; the header prints the seed). `--no-shuffle` keeps\n\
sorted path order and source order."
)]
struct TestCli {
    #[command(flatten)]
    log: LogFlags,
    #[command(flatten)]
    opt: OptLevelFlags,
    #[command(flatten)]
    grants: HostGrantFlags,
    #[command(flatten)]
    roots: RootFlags,
    /// Stop after the first failed case
    #[arg(long)]
    fail_fast: bool,
    /// Shuffle files and cases with seed N (decimal or 0x hex)
    #[arg(long, value_name = "N", value_parser = parse_seed)]
    seed: Option<u64>,
    /// Run files in sorted path order and cases in source order
    #[arg(long)]
    no_shuffle: bool,
    /// Run cases on N reactor workers (default: available CPUs)
    #[arg(short = 'j', long = "jobs", value_name = "N", value_parser = parse_jobs)]
    jobs: Option<usize>,
    /// Also print passing cases' output (failures always show it)
    #[arg(long)]
    show_output: bool,
    /// Line coverage of project sources: lcov + summary
    #[arg(long)]
    coverage: bool,
    /// lcov path (default target/coverage/lcov.info; implies --coverage)
    #[arg(long, value_name = "FILE")]
    coverage_out: Option<PathBuf>,
    /// Also write test -> file -> lines JSON (implies --coverage)
    #[arg(long, value_name = "FILE")]
    coverage_per_test: Option<PathBuf>,
    /// NDJSON events on stdout (start, file, summary, error)
    #[arg(long)]
    json: bool,
    /// Test root (default: ./tests)
    #[arg(value_name = "PATH")]
    path: Option<String>,
}

#[derive(Parser, Debug)]
#[command(
    name = "coil-test",
    about = "Change project sources one small edit at a time and check that some test fails",
    disable_help_subcommand = true,
    after_help = "Runs the suite once with coverage (it must pass), then each mutant against\n\
only the cases that cover its line. Sources under the test root are not\n\
mutated unless --files selects them. `// coil:no-mutate` on a line (or on /\n\
above a `fn` header) skips it.\n\
\n\
`--operators` is a comma-separated subset of: boundary, negate, arith,\n\
logic, cond, bool, int (default: all). `--seed`, `-j`, `-O`, `--root`,\n\
`--allow-*`, and `--log-*` match `coil test`."
)]
struct MutateCli {
    #[command(flatten)]
    log: LogFlags,
    #[command(flatten)]
    opt: OptLevelFlags,
    #[command(flatten)]
    grants: HostGrantFlags,
    #[command(flatten)]
    roots: RootFlags,
    /// Only mutate sources matching GLOB (relative path; `*`, `**`, `?`; repeatable)
    #[arg(long = "files", value_name = "GLOB", action = clap::ArgAction::Append)]
    files: Vec<String>,
    /// Comma-separated subset of boundary, negate, arith, logic, cond, bool, int
    #[arg(long, value_name = "LIST")]
    operators: Option<String>,
    /// Step budget per case = N x its baseline steps (default 10)
    #[arg(
        long,
        value_name = "N",
        default_value_t = DEFAULT_TIMEOUT_FACTOR,
        value_parser = parse_timeout_factor
    )]
    timeout_factor: u64,
    /// Kill a mutant's worker process after S seconds (default 60)
    #[arg(
        long,
        value_name = "S",
        default_value_t = DEFAULT_WALL_TIMEOUT,
        value_parser = parse_wall_timeout
    )]
    wall_timeout: u64,
    /// Exit 1 when the mutation score is below P percent
    #[arg(long, value_name = "P", value_parser = parse_min_score)]
    min_score: Option<f64>,
    /// NDJSON events on stdout (plan, mutant, summary, error)
    #[arg(long)]
    json: bool,
    /// Shuffle files and cases with seed N (decimal or 0x hex)
    #[arg(long, value_name = "N", value_parser = parse_seed)]
    seed: Option<u64>,
    /// Run files in sorted path order and cases in source order
    #[arg(long)]
    no_shuffle: bool,
    /// Run cases on N reactor workers (default: available CPUs)
    #[arg(short = 'j', long = "jobs", value_name = "N", value_parser = parse_jobs)]
    jobs: Option<usize>,
    /// Test root (default: ./tests)
    #[arg(value_name = "PATH")]
    path: Option<String>,
}

fn named(mut command: Command, bin_name: &str) -> Command {
    command.set_bin_name(bin_name);
    command
}

pub fn print_mutate_help() {
    print_command_help(named(MutateCli::command(), "coil mutate"), "coil mutate");
}

pub fn print_help() {
    print_command_help(named(TestCli::command(), "coil test"), "coil test");
}

/// Parse argv (including argv0). A first argument of [`MUTATE`] selects
/// `coil mutate`.
pub fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let expanded = expand_o_shorts(args);
    if expanded.get(1).map(String::as_str) == Some(MUTATE) {
        return parse_mutate(&expanded, args);
    }
    parse_test(&expanded)
}

fn parse_test(args: &[String]) -> Result<Parsed, String> {
    let Some(cli) = parse_with::<TestCli>(named(TestCli::command(), "coil test"), args)? else {
        return Ok(Parsed::Help);
    };
    let (config, test) = assemble(RunParts {
        log: cli.log,
        opt: cli.opt,
        grants: cli.grants,
        roots: cli.roots,
        seed: cli.seed,
        no_shuffle: cli.no_shuffle,
        jobs: cli.jobs,
        json: cli.json,
        path: cli.path,
        fail_fast: cli.fail_fast,
        show_output: cli.show_output,
        coverage: cli.coverage,
        coverage_out: cli.coverage_out,
        per_test_out: cli.coverage_per_test,
    })?;
    Ok(Parsed::Run(config, test))
}

fn parse_mutate(expanded: &[String], original: &[String]) -> Result<Parsed, String> {
    let mut clap_args = Vec::with_capacity(expanded.len().saturating_sub(1));
    if let Some(argv0) = expanded.first() {
        clap_args.push(argv0.clone());
    }
    clap_args.extend_from_slice(&expanded[2..]);
    let Some(cli) =
        parse_with::<MutateCli>(named(MutateCli::command(), "coil mutate"), &clap_args)?
    else {
        return Ok(Parsed::MutateHelp);
    };
    let (config, test) = assemble(RunParts {
        log: cli.log,
        opt: cli.opt,
        grants: cli.grants,
        roots: cli.roots,
        seed: cli.seed,
        no_shuffle: cli.no_shuffle,
        jobs: cli.jobs,
        json: cli.json,
        path: cli.path,
        fail_fast: false,
        show_output: false,
        coverage: false,
        coverage_out: None,
        per_test_out: None,
    })?;
    let forwarded = original.get(2..).unwrap_or(&[]).to_vec();
    let operators = match cli.operators {
        Some(list) => parse_operators(&list)?,
        None => Operator::ALL.to_vec(),
    };
    Ok(Parsed::Mutate(
        config,
        Box::new(MutateOptions {
            test: *test,
            files: cli.files,
            operators,
            timeout_factor: cli.timeout_factor,
            json: cli.json,
            min_score: cli.min_score,
            project_root: std::env::current_dir().unwrap_or_default(),
            isolation: Isolation::Child {
                exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("coil-test")),
                args: forwarded,
                wall: std::time::Duration::from_secs(cli.wall_timeout),
            },
        }),
    ))
}

/// Shared `coil test` / `coil mutate` options after clap has parsed them.
struct RunParts {
    log: LogFlags,
    opt: OptLevelFlags,
    grants: HostGrantFlags,
    roots: RootFlags,
    seed: Option<u64>,
    no_shuffle: bool,
    jobs: Option<usize>,
    json: bool,
    path: Option<String>,
    fail_fast: bool,
    show_output: bool,
    coverage: bool,
    coverage_out: Option<PathBuf>,
    per_test_out: Option<PathBuf>,
}

fn assemble(parts: RunParts) -> Result<(ReportConfig, Box<TestOptions>), String> {
    if parts.json && (parts.log.log_json || parts.log.log_lsp) {
        return Err("--json cannot be combined with --log-json or --log-lsp".into());
    }
    let config = ReportConfig::from_cli_flags(parts.log.log_json, parts.log.log_lsp)
        .map_err(|e| e.to_string())?;
    let env_seed = std::env::var(SEED_ENV).ok();
    let order = resolve_order(
        parts.seed,
        parts.no_shuffle,
        env_seed.as_deref(),
        fresh_seed,
    )?;
    Ok((
        config,
        Box::new(TestOptions {
            root: PathBuf::from(parts.path.unwrap_or_else(|| TESTS_DIR.to_string())),
            fail_fast: parts.fail_fast,
            order,
            jobs: parts.jobs.unwrap_or_else(default_jobs),
            show_output: parts.show_output,
            coverage: (parts.coverage
                || parts.coverage_out.is_some()
                || parts.per_test_out.is_some())
            .then(|| CoverageOptions {
                lcov_out: parts
                    .coverage_out
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_LCOV_OUT)),
                per_test_out: parts.per_test_out,
                project_root: std::env::current_dir().unwrap_or_default(),
            }),
            opt_level: parts.opt.level(),
            grants: parts.grants.into_grants(),
            extra_roots: parts.roots.root,
            report: if parts.json {
                Report::Json
            } else {
                Report::Human
            },
        }),
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

fn parse_operators(list: &str) -> Result<Vec<Operator>, String> {
    let mut ops = Vec::new();
    for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        ops.push(Operator::parse(name).ok_or_else(|| {
            format!("unknown mutation operator `{name}` (see `coil mutate --help`)")
        })?);
    }
    Ok(ops)
}

fn parse_timeout_factor(v: &str) -> Result<u64, String> {
    match v.trim().parse::<u64>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "invalid --timeout-factor `{v}` (expected at least 1)"
        )),
    }
}

fn parse_wall_timeout(v: &str) -> Result<u64, String> {
    match v.trim().parse::<u64>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "invalid --wall-timeout `{v}` (expected seconds >= 1)"
        )),
    }
}

fn parse_min_score(v: &str) -> Result<f64, String> {
    v.trim()
        .parse::<f64>()
        .ok()
        .filter(|p| (0.0..=100.0).contains(p))
        .ok_or_else(|| format!("invalid --min-score `{v}` (expected 0..=100)"))
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
    use compiler::{HostGrants, OptLevel};

    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("coil-test".to_string())
            .chain(parts.iter().map(|s| (*s).to_string()))
            .collect()
    }

    fn run(parts: &[&str]) -> (ReportConfig, TestOptions) {
        match parse_args(&argv(parts)).expect("parses") {
            Parsed::Run(config, options) => (config, *options),
            _ => panic!("expected a test run"),
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

    fn mutate(parts: &[&str]) -> MutateOptions {
        let mut all = vec![MUTATE];
        all.extend_from_slice(parts);
        match parse_args(&argv(&all)).expect("parses") {
            Parsed::Mutate(_, options) => *options,
            _ => panic!("expected mutate"),
        }
    }

    #[test]
    fn mutate_splits_its_flags_from_test_flags() {
        let m = mutate(&[
            "--files",
            "src/**",
            "--operators=arith, bool",
            "--seed",
            "7",
            "-j",
            "2",
            "--timeout-factor",
            "4",
            "--min-score=80",
            "--json",
            "spec",
        ]);
        assert_eq!(m.files, ["src/**"]);
        assert_eq!(m.operators, [Operator::Arith, Operator::Bool]);
        assert_eq!(m.timeout_factor, 4);
        assert_eq!(m.min_score, Some(80.0));
        assert!(m.json);
        assert_eq!(m.test.order, Order::Shuffled(7));
        assert_eq!(m.test.jobs, 2);
        assert_eq!(m.test.root, PathBuf::from("spec"));
        assert!(m.test.coverage.is_none());
        match &m.isolation {
            Isolation::Child { args, wall, .. } => {
                assert_eq!(args.len(), 12, "every flag is forwarded to the worker");
                assert_eq!(*wall, std::time::Duration::from_secs(DEFAULT_WALL_TIMEOUT));
            }
            Isolation::InProcess => panic!("the CLI isolates mutants"),
        }

        let d = mutate(&[]);
        assert_eq!(d.operators, Operator::ALL);
        assert_eq!(d.timeout_factor, DEFAULT_TIMEOUT_FACTOR);
        assert_eq!(d.test.root, PathBuf::from(TESTS_DIR));
    }

    #[test]
    fn mutate_rejects_bad_values_and_test_only_flags() {
        for bad in [
            &["--operators", "swap"][..],
            &["--timeout-factor", "0"],
            &["--wall-timeout", "0"],
            &["--min-score", "120"],
            &["--files"],
            &["--coverage"],
            &["--fail-fast"],
            &["--bogus"],
        ] {
            let mut all = vec![MUTATE];
            all.extend_from_slice(bad);
            assert!(parse_args(&argv(&all)).is_err(), "{bad:?}");
        }
        assert!(matches!(
            parse_args(&argv(&[MUTATE, "--json", "--help"])),
            Ok(Parsed::MutateHelp)
        ));
    }
}
