//! `coil` argv: clap parser plus the command enum `main` dispatches on.

use std::path::{Path, PathBuf};
use std::process::exit;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use compiler::OptLevel;

pub(crate) const DEFAULT_OUT: &str = "out.hyc";

const RESERVED: &[&str] = &[
    "compile", "run", "test", "package", "dissect", "debug", "fmt", "lsp",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Default: compile entry in memory and run (no out.hyc).
    BuildAndRun {
        filename: String,
    },
    Compile {
        filename: String,
        output: String,
    },
    Run {
        archive: String,
    },
    Test {
        path: Option<String>,
        fail_fast: bool,
    },
    Package {
        filename: String,
        output: String,
        runner: Option<PathBuf>,
        check_native: bool,
        strip_debug: bool,
    },
    Dissect {
        filename: String,
        fn_pat: Option<String>,
        show_il: bool,
        show_ast: bool,
    },
    Debug {
        filename: Option<String>,
        script: Option<String>,
        batch: bool,
        dap: bool,
    },
    /// Re-exec `coil-fmt` (paths / `--check` forwarded via argv).
    Fmt,
    /// Re-exec `coil-lsp` (LSP transport runs over stdin/stdout).
    Lsp,
    /// Print the toolchain version (`CARGO_PKG_VERSION`) and exit.
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliArgs {
    pub command: Command,
    pub log_json: bool,
    pub log_lsp: bool,
    pub include_tests: bool,
    pub opt_level: OptLevel,
    pub opt_stats: bool,
    pub opt_stats_json: bool,
    pub pgo_instrument: bool,
    pub pgo_use_profile: Option<String>,
    pub pgo_generate_profile: Option<String>,
}

#[derive(Parser, Debug)]
#[command(
    name = "coil",
    version,
    about = "Coil compiler and runtime",
    disable_help_subcommand = true,
    after_help = "When no file is given, `coil` / `coil compile` use `[entry].file` from coil.toml.\n\
Default diagnostics: pretty reports on stderr."
)]
struct RawCli {
    #[command(flatten)]
    g: Globals,
    /// Compile this `.hy` file in memory and run it (or `[entry].file` from coil.toml)
    #[arg(value_name = "FILE")]
    file: Option<String>,
    #[command(subcommand)]
    command: Option<RawCommand>,
}

#[derive(Args, Debug)]
#[command(next_help_heading = "Options")]
struct Globals {
    /// Emit SARIF 2.1 diagnostics on stdout
    #[arg(long, global = true)]
    log_json: bool,
    /// Emit LSP Diagnostic NDJSON on stdout
    #[arg(long, global = true)]
    log_lsp: bool,
    /// Accepted for LSP clients; ignored
    #[arg(long, global = true, hide = true)]
    stdio: bool,
    /// With `test`, stop after the first failed case
    #[arg(long, global = true)]
    fail_fast: bool,
    /// Compile harness tests into the archive (default: omit)
    #[arg(long, global = true)]
    include_tests: bool,
    /// Output archive for `compile` or packaged binary for `package`
    #[arg(short = 'o', long = "output", global = true, value_name = "PATH")]
    output: Option<String>,
    /// none/0, basic/1, standard/2 (default), aggressive/3, size/s, debug/g
    #[arg(
        short = 'O',
        long = "opt-level",
        global = true,
        value_name = "LEVEL",
        value_parser = parse_opt_level
    )]
    opt_level: Option<OptLevel>,
    /// Print IL optimization counters after compile (stderr)
    #[arg(long, global = true)]
    opt_stats: bool,
    /// Print the same counters as one JSON object (stderr)
    #[arg(long, global = true)]
    opt_stats_json: bool,
    /// Insert pgo_hit counters at function/block/branch sites
    #[arg(long, global = true)]
    pgo_instrument: bool,
    /// Apply a JSON profile to layout and inlining
    #[arg(long, global = true, value_name = "FILE")]
    pgo_use_profile: Option<String>,
    /// Write runtime or current profile JSON
    #[arg(long, global = true, value_name = "FILE")]
    pgo_generate_profile: Option<String>,
    /// Runner template for `package` (default: `coil-embed` beside this binary)
    #[arg(long, global = true, value_name = "PATH")]
    runner: Option<PathBuf>,
    /// With `package`, fail if required shared libraries are missing
    #[arg(long, global = true)]
    check_native: bool,
    /// With `package`, omit debug line table from embedded archive
    #[arg(long, global = true)]
    strip_debug: bool,
    /// With `dissect`, filter functions by FQN substring / trailing name
    #[arg(long = "fn", global = true, value_name = "PAT")]
    fn_pat: Option<String>,
    /// With `dissect`, also print pre-opt stack IL
    #[arg(long, global = true)]
    il: bool,
    /// With `dissect`, also print the entry-file AST
    #[arg(long, global = true)]
    ast: bool,
    /// With `debug`, run commands from a script file
    #[arg(short = 'x', global = true, value_name = "SCRIPT")]
    script: Option<String>,
    /// With `debug`, non-interactive (use -x or stdin); exit after script
    #[arg(long, global = true)]
    batch: bool,
    /// With `debug`, Debug Adapter Protocol over stdio
    #[arg(long, global = true)]
    dap: bool,
    /// With `fmt`, exit 1 if files would change (no writes)
    #[arg(long, global = true)]
    check: bool,
}

#[derive(Subcommand, Debug)]
enum RawCommand {
    /// Compile an entry file (must define main) to a .hyc archive
    Compile {
        /// Entry `.hy` (or `[entry].file` from coil.toml)
        file: Option<String>,
    },
    /// Execute a previously compiled .hyc archive
    Run {
        /// Archive path
        archive: String,
    },
    /// Compile and run every .hy file under [path] (default: ./tests)
    Test {
        /// Test root (files under `compile_fail/` must be rejected)
        path: Option<String>,
    },
    /// Build a single-host executable (runner + embedded .hyc)
    Package {
        /// Entry `.hy` file
        file: String,
    },
    /// In-memory compile and dump filtered bytecode / IL / AST
    Dissect {
        /// Entry `.hy` file
        file: String,
    },
    /// GDB-style debugger (REPL; --dap for IDE)
    Debug {
        /// Entry `.hy` file (omit with `--dap`)
        file: Option<String>,
    },
    /// Format `.hy` sources (re-execs `coil-fmt`)
    Fmt {
        /// Files or directories
        #[arg(required = true, trailing_var_arg = true)]
        paths: Vec<String>,
    },
    /// Start the Coil language server over stdin/stdout
    Lsp,
}

fn parse_opt_level(s: &str) -> Result<OptLevel, String> {
    OptLevel::parse(s).map_err(|_| {
        "invalid --opt-level (expected none|basic|standard|aggressive|size|debug or 0|1|2|3|s|g)"
            .into()
    })
}

fn expand_o_shorts(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        if a.starts_with("-O") && a.len() > 2 && !a.starts_with("--") {
            out.push("-O".into());
            out.push(a[2..].into());
        } else {
            out.push(a.clone());
        }
    }
    out
}

fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name)
}

/// Parse argv (including argv0). `-V` / `--version` win over every other token.
pub(crate) fn parse_args(args: &[String]) -> Result<CliArgs, String> {
    if args.iter().skip(1).any(|a| a == "-V" || a == "--version") {
        return Ok(CliArgs {
            command: Command::Version,
            log_json: false,
            log_lsp: false,
            include_tests: false,
            opt_level: OptLevel::Standard,
            opt_stats: false,
            opt_stats_json: false,
            pgo_instrument: false,
            pgo_use_profile: None,
            pgo_generate_profile: None,
        });
    }

    let expanded = expand_o_shorts(args);
    let matches = match RawCli::command()
        .bin_name("coil")
        .try_get_matches_from(&expanded)
    {
        Ok(m) => m,
        Err(e) => {
            if e.kind() == clap::error::ErrorKind::DisplayHelp
                || e.kind() == clap::error::ErrorKind::DisplayVersion
            {
                let _ = e.print();
                exit(e.exit_code());
            }
            return Err(format_clap_error(e));
        }
    };
    let raw = RawCli::from_arg_matches(&matches).map_err(|e| format_clap_error(e))?;
    raw.into_cli_args()
}

fn format_clap_error(e: clap::Error) -> String {
    let msg = e.to_string();
    let line = msg
        .lines()
        .find(|l| !l.is_empty() && !l.starts_with("Usage:"))
        .unwrap_or(msg.trim());
    let line = line.trim().trim_end_matches(':');
    if line.contains("unexpected argument") {
        format!(
            "{line} (expected --log-json, --log-lsp, --stdio, --fail-fast, --include-tests, \
             --opt-stats, --opt-stats-json, --pgo-instrument, --pgo-use-profile, \
             --pgo-generate-profile, --check-native, --strip-debug, --runner, --fn, --il, --ast, \
             -x, --batch, --check, -o/--output, -O/--opt-level, -V/--version, or a command/file)"
        )
    } else {
        line.to_string()
    }
}

impl RawCli {
    fn into_cli_args(self) -> Result<CliArgs, String> {
        let g = self.g;
        let has_dissect = g.fn_pat.is_some() || g.il || g.ast;
        let has_debug = g.script.is_some() || g.batch || g.dap;
        let has_fmt = g.check;
        let has_package = g.check_native || g.strip_debug || g.runner.is_some();

        let command = match self.command {
            None => {
                reject_output(&g)?;
                reject_fail_fast(&g)?;
                reject_package_flags(has_package)?;
                reject_dissect(has_dissect)?;
                reject_debug(has_debug)?;
                reject_fmt(has_fmt)?;
                Command::BuildAndRun {
                    filename: self.file.unwrap_or_default(),
                }
            }
            Some(RawCommand::Lsp) => {
                if self.file.is_some() {
                    return Err("too many arguments".into());
                }
                if g.output.is_some()
                    || g.fail_fast
                    || g.include_tests
                    || g.opt_stats
                    || g.opt_stats_json
                    || g.log_json
                    || g.log_lsp
                    || has_package
                    || has_dissect
                    || has_debug
                    || has_fmt
                {
                    return Err("flags other than --stdio are not valid with `lsp`".into());
                }
                Command::Lsp
            }
            Some(RawCommand::Fmt { paths }) => {
                if self.file.is_some() {
                    return Err("too many arguments".into());
                }
                if paths.is_empty() {
                    return Err("fmt requires at least one file or directory".into());
                }
                if g.output.is_some() {
                    return Err("-o/--output is not valid with `fmt`".into());
                }
                reject_fail_fast(&g)?;
                if g.include_tests {
                    return Err("--include-tests is not valid with `fmt`".into());
                }
                reject_package_flags(has_package)?;
                reject_dissect(has_dissect)?;
                reject_debug(has_debug)?;
                Command::Fmt
            }
            Some(RawCommand::Test { path }) => {
                extra_file(self.file.as_deref())?;
                reject_output(&g)?;
                if g.include_tests {
                    return Err(
                        "--include-tests is only valid with `compile` or the default run mode"
                            .into(),
                    );
                }
                reject_package_flags(has_package)?;
                reject_dissect(has_dissect)?;
                reject_debug(has_debug)?;
                reject_fmt(has_fmt)?;
                if let Some(p) = &path {
                    if is_reserved(p) {
                        return Err("test path must be a directory".into());
                    }
                }
                Command::Test {
                    path,
                    fail_fast: g.fail_fast,
                }
            }
            Some(RawCommand::Compile { file }) => {
                extra_file(self.file.as_deref())?;
                reject_fail_fast(&g)?;
                reject_package_flags(has_package)?;
                reject_dissect(has_dissect)?;
                reject_debug(has_debug)?;
                reject_fmt(has_fmt)?;
                let filename = file.unwrap_or_default();
                if !filename.is_empty() && is_reserved(&filename) {
                    return Err("compile requires an entry file".into());
                }
                Command::Compile {
                    filename,
                    output: g.output.unwrap_or_else(|| DEFAULT_OUT.to_string()),
                }
            }
            Some(RawCommand::Run { archive }) => {
                extra_file(self.file.as_deref())?;
                reject_output(&g)?;
                reject_fail_fast(&g)?;
                reject_package_flags(has_package)?;
                reject_dissect(has_dissect)?;
                reject_debug(has_debug)?;
                reject_fmt(has_fmt)?;
                Command::Run { archive }
            }
            Some(RawCommand::Package { file }) => {
                extra_file(self.file.as_deref())?;
                if is_reserved(&file) {
                    return Err("package requires an entry file".into());
                }
                reject_fail_fast(&g)?;
                if g.include_tests {
                    return Err("--include-tests is not valid with `package`".into());
                }
                reject_dissect(has_dissect)?;
                reject_debug(has_debug)?;
                reject_fmt(has_fmt)?;
                let out = g.output.unwrap_or_else(|| {
                    Path::new(&file)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("a.out")
                        .to_string()
                });
                Command::Package {
                    filename: file,
                    output: out,
                    runner: g.runner,
                    check_native: g.check_native,
                    strip_debug: g.strip_debug,
                }
            }
            Some(RawCommand::Dissect { file }) => {
                extra_file(self.file.as_deref())?;
                if is_reserved(&file) {
                    return Err("dissect requires an entry .hy file".into());
                }
                reject_output(&g)?;
                reject_fail_fast(&g)?;
                if g.include_tests {
                    return Err("--include-tests is not valid with `dissect`".into());
                }
                reject_package_flags(has_package)?;
                reject_debug(has_debug)?;
                reject_fmt(has_fmt)?;
                Command::Dissect {
                    filename: file,
                    fn_pat: g.fn_pat,
                    show_il: g.il,
                    show_ast: g.ast,
                }
            }
            Some(RawCommand::Debug { file }) => {
                extra_file(self.file.as_deref())?;
                if g.dap && file.is_none() {
                    Command::Debug {
                        filename: None,
                        script: None,
                        batch: false,
                        dap: true,
                    }
                } else {
                    let Some(filename) = file else {
                        return Err("debug requires an entry .hy file (or use --dap)".into());
                    };
                    if is_reserved(&filename) {
                        return Err("debug requires an entry .hy file".into());
                    }
                    reject_output(&g)?;
                    reject_fail_fast(&g)?;
                    if g.include_tests {
                        return Err("--include-tests is not valid with `debug`".into());
                    }
                    reject_package_flags(has_package)?;
                    reject_dissect(has_dissect)?;
                    if g.dap {
                        return Err("--dap cannot be combined with a positional .hy file".into());
                    }
                    Command::Debug {
                        filename: Some(filename),
                        script: g.script,
                        batch: g.batch,
                        dap: g.dap,
                    }
                }
            }
        };

        if (g.opt_stats || g.opt_stats_json)
            && matches!(
                command,
                Command::Run { .. }
                    | Command::Test { .. }
                    | Command::Fmt
                    | Command::Lsp
                    | Command::Debug { .. }
            )
        {
            return Err("--opt-stats and --opt-stats-json require compiling source".into());
        }
        if (g.pgo_instrument || g.pgo_use_profile.is_some() || g.pgo_generate_profile.is_some())
            && matches!(
                command,
                Command::Run { .. }
                    | Command::Test { .. }
                    | Command::Fmt
                    | Command::Lsp
                    | Command::Debug { .. }
            )
        {
            return Err("PGO flags require compiling source".into());
        }

        Ok(CliArgs {
            command,
            log_json: g.log_json,
            log_lsp: g.log_lsp,
            include_tests: g.include_tests,
            opt_level: g.opt_level.unwrap_or(OptLevel::Standard),
            opt_stats: g.opt_stats,
            opt_stats_json: g.opt_stats_json,
            pgo_instrument: g.pgo_instrument,
            pgo_use_profile: g.pgo_use_profile,
            pgo_generate_profile: g.pgo_generate_profile,
        })
    }
}

fn extra_file(file: Option<&str>) -> Result<(), String> {
    if file.is_some() {
        Err("too many arguments".into())
    } else {
        Ok(())
    }
}

fn reject_output(g: &Globals) -> Result<(), String> {
    if g.output.is_some() {
        Err("-o/--output is only valid with `compile` or `package`".into())
    } else {
        Ok(())
    }
}

fn reject_fail_fast(g: &Globals) -> Result<(), String> {
    if g.fail_fast {
        Err("--fail-fast is only valid with `test`".into())
    } else {
        Ok(())
    }
}

fn reject_package_flags(has: bool) -> Result<(), String> {
    if has {
        Err("--check-native, --strip-debug, and --runner are only valid with `package`".into())
    } else {
        Ok(())
    }
}

fn reject_dissect(has: bool) -> Result<(), String> {
    if has {
        Err("--fn, --il, and --ast are only valid with `dissect`".into())
    } else {
        Ok(())
    }
}

fn reject_debug(has: bool) -> Result<(), String> {
    if has {
        Err("-x and --batch are only valid with `debug`".into())
    } else {
        Ok(())
    }
}

fn reject_fmt(has: bool) -> Result<(), String> {
    if has {
        Err("--check is only valid with `fmt`".into())
    } else {
        Ok(())
    }
}

pub(crate) fn print_version() {
    println!("coil {}", env!("CARGO_PKG_VERSION"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        std::iter::once("coil".to_string())
            .chain(parts.iter().map(|s| (*s).to_string()))
            .collect()
    }

    #[test]
    fn parse_version_long_and_short() {
        let cli = parse_args(&args(&["--version"])).unwrap();
        assert_eq!(cli.command, Command::Version);
        let cli = parse_args(&args(&["-V"])).unwrap();
        assert_eq!(cli.command, Command::Version);
    }

    #[test]
    fn parse_version_wins_over_other_args() {
        let cli = parse_args(&args(&["compile", "a.hy", "--version"])).unwrap();
        assert_eq!(cli.command, Command::Version);
        let cli = parse_args(&args(&["--help", "--version"])).unwrap();
        assert_eq!(cli.command, Command::Version);
        let cli = parse_args(&args(&["-V", "--help"])).unwrap();
        assert_eq!(cli.command, Command::Version);
    }

    #[test]
    fn parse_fmt_paths() {
        let cli = parse_args(&args(&["fmt", "a.hy", "src/"])).unwrap();
        assert_eq!(cli.command, Command::Fmt);
    }

    #[test]
    fn parse_fmt_with_check() {
        let cli = parse_args(&args(&["fmt", "--check", "a.hy"])).unwrap();
        assert_eq!(cli.command, Command::Fmt);
    }

    #[test]
    fn parse_lsp_with_stdio() {
        let cli = parse_args(&args(&["--stdio", "lsp"])).unwrap();
        assert_eq!(cli.command, Command::Lsp);
    }

    #[test]
    fn parse_rejects_check_on_non_fmt() {
        assert!(parse_args(&args(&["compile", "a.hy", "--check"])).is_err());
        assert!(parse_args(&args(&["--check", "a.hy"])).is_err());
    }

    #[test]
    fn parse_debug_with_script_batch() {
        let cli = parse_args(&args(&[
            "debug",
            "examples/fib.hy",
            "-x",
            "cmds.txt",
            "--batch",
        ]))
        .unwrap();
        assert_eq!(
            cli.command,
            Command::Debug {
                filename: Some("examples/fib.hy".into()),
                script: Some("cmds.txt".into()),
                batch: true,
                dap: false,
            }
        );
    }

    #[test]
    fn parse_dissect_with_fn_il_ast() {
        let cli = parse_args(&args(&[
            "dissect",
            "examples/fib.hy",
            "--fn",
            "fib",
            "--il",
            "--ast",
        ]))
        .unwrap();
        assert_eq!(
            cli.command,
            Command::Dissect {
                filename: "examples/fib.hy".into(),
                fn_pat: Some("fib".into()),
                show_il: true,
                show_ast: true,
            }
        );
    }

    #[test]
    fn parse_legacy_build_and_run() {
        let cli = parse_args(&args(&["examples/fib.hy"])).unwrap();
        assert_eq!(
            cli.command,
            Command::BuildAndRun {
                filename: "examples/fib.hy".into()
            }
        );
        assert!(!cli.log_json);
    }

    #[test]
    fn parse_compile_default_output() {
        let cli = parse_args(&args(&["compile", "examples/fib.hy"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Compile {
                filename: "examples/fib.hy".into(),
                output: DEFAULT_OUT.into(),
            }
        );
    }

    #[test]
    fn parse_compile_with_short_output() {
        let cli = parse_args(&args(&["compile", "examples/fib.hy", "-o", "fib.hyc"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Compile {
                filename: "examples/fib.hy".into(),
                output: "fib.hyc".into(),
            }
        );
    }

    #[test]
    fn parse_compile_with_long_output_before_command() {
        let cli = parse_args(&args(&["--output", "x.hyc", "compile", "a.hy"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Compile {
                filename: "a.hy".into(),
                output: "x.hyc".into(),
            }
        );
    }

    #[test]
    fn parse_run() {
        let cli = parse_args(&args(&["run", "out.hyc"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Run {
                archive: "out.hyc".into()
            }
        );
    }

    #[test]
    fn parse_package_default_output() {
        let cli = parse_args(&args(&["package", "examples/fib.hy"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Package {
                filename: "examples/fib.hy".into(),
                output: "fib".into(),
                runner: None,
                check_native: false,
                strip_debug: false,
            }
        );
    }

    #[test]
    fn parse_package_with_flags() {
        let cli = parse_args(&args(&[
            "package",
            "app.hy",
            "-o",
            "myapp",
            "--check-native",
            "--strip-debug",
            "--runner",
            "/usr/bin/coil",
        ]))
        .unwrap();
        assert_eq!(
            cli.command,
            Command::Package {
                filename: "app.hy".into(),
                output: "myapp".into(),
                runner: Some(PathBuf::from("/usr/bin/coil")),
                check_native: true,
                strip_debug: true,
            }
        );
    }

    #[test]
    fn parse_test() {
        let cli = parse_args(&args(&["test"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Test {
                path: None,
                fail_fast: false,
            }
        );
    }

    #[test]
    fn parse_test_with_path_and_fail_fast() {
        let cli = parse_args(&args(&["test", "./tests", "--fail-fast"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Test {
                path: Some("./tests".into()),
                fail_fast: true,
            }
        );
        let cli = parse_args(&args(&["--fail-fast", "test"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Test {
                path: None,
                fail_fast: true,
            }
        );
    }

    #[test]
    fn parse_log_flags_with_subcommand() {
        let cli = parse_args(&args(&["--log-json", "compile", "a.hy"])).unwrap();
        assert!(cli.log_json);
        assert!(matches!(cli.command, Command::Compile { .. }));

        let cli = parse_args(&args(&["test", "--log-lsp"])).unwrap();
        assert!(cli.log_lsp);
        assert_eq!(
            cli.command,
            Command::Test {
                path: None,
                fail_fast: false,
            }
        );
    }

    #[test]
    fn parse_rejects_output_on_run_and_test() {
        assert!(parse_args(&args(&["run", "a.hyc", "-o", "x"])).is_err());
        assert!(parse_args(&args(&["test", "-o", "x"])).is_err());
        assert!(parse_args(&args(&["examples/fib.hy", "-o", "x"])).is_err());
        assert!(parse_args(&args(&["test", "--include-tests"])).is_err());
    }

    #[test]
    fn parse_empty_args_defers_to_manifest_entry() {
        let cli = parse_args(&args(&[])).unwrap();
        assert_eq!(
            cli.command,
            Command::BuildAndRun {
                filename: String::new()
            }
        );
    }

    #[test]
    fn parse_compile_without_file_defers_to_manifest_entry() {
        let cli = parse_args(&args(&["compile"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Compile {
                filename: String::new(),
                output: DEFAULT_OUT.into(),
            }
        );
    }

    #[test]
    fn parse_rejects_missing_run_archive() {
        assert!(parse_args(&args(&["run"])).is_err());
    }

    #[test]
    fn parse_rejects_fail_fast_on_non_test_commands() {
        assert!(parse_args(&args(&["--fail-fast", "examples/fib.hy"])).is_err());
        assert!(parse_args(&args(&["compile", "a.hy", "--fail-fast"])).is_err());
        assert!(parse_args(&args(&["run", "out.hyc", "--fail-fast"])).is_err());
    }

    #[test]
    fn parse_rejects_reserved_test_path_names() {
        assert!(parse_args(&args(&["test", "compile"])).is_err());
        assert!(parse_args(&args(&["test", "run"])).is_err());
        assert!(parse_args(&args(&["test", "test"])).is_err());
    }

    #[test]
    fn parse_rejects_unrecognized_flag() {
        let err = parse_args(&args(&["--bogus", "a.hy"])).unwrap_err();
        assert!(
            err.contains("unrecognized") || err.contains("unexpected"),
            "{err}"
        );
        assert!(err.contains("--version"), "{err}");
    }

    #[test]
    fn parse_rejects_duplicate_output_and_missing_output_path() {
        assert!(parse_args(&args(&["compile", "a.hy", "-o"])).is_err());
        assert!(parse_args(&args(&["compile", "a.hy", "-o", "-x"])).is_err());
        assert!(parse_args(&args(&["compile", "a.hy", "-o", "x", "--output", "y"])).is_err());
    }

    #[test]
    fn parse_rejects_too_many_args_and_reserved_compile_names() {
        assert!(parse_args(&args(&["a.hy", "b.hy"])).is_err());
        assert!(parse_args(&args(&["compile", "compile"])).is_err());
        assert!(parse_args(&args(&["compile", "run"])).is_err());
        assert!(parse_args(&args(&["compile", "test"])).is_err());
    }

    #[test]
    fn parse_accepts_both_log_flags_at_parse_time() {
        let cli = parse_args(&args(&["--log-json", "--log-lsp", "test"])).unwrap();
        assert!(cli.log_json && cli.log_lsp);

        let cli = parse_args(&args(&["--include-tests", "examples/fib.hy"])).unwrap();
        assert!(cli.include_tests);
        assert!(matches!(cli.command, Command::BuildAndRun { .. }));
        assert_eq!(cli.opt_level, OptLevel::Standard);
    }

    #[test]
    fn parse_opt_level_flags() {
        let cli = parse_args(&args(&["-O0", "examples/fib.hy"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::None);
        let cli = parse_args(&args(&["-O", "2", "examples/fib.hy"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::Standard);
        let cli = parse_args(&args(&["--opt-level", "aggressive", "compile", "a.hy"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::Aggressive);
        let cli = parse_args(&args(&["--opt-level=size", "a.hy"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::Size);
        let cli = parse_args(&args(&["-Og", "a.hy"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::Debug);
        assert!(parse_args(&args(&["-O9", "a.hy"])).is_err());
        assert!(parse_args(&args(&["-O2", "-O0", "a.hy"])).is_err());
        assert!(parse_args(&args(&["--opt-level"])).is_err());
    }

    #[test]
    fn parse_opt_stats_flags() {
        let cli = parse_args(&args(&["--opt-stats", "compile", "a.hy"])).unwrap();
        assert!(cli.opt_stats && !cli.opt_stats_json);
        let cli = parse_args(&args(&["--opt-stats-json", "a.hy"])).unwrap();
        assert!(cli.opt_stats_json && !cli.opt_stats);
        let cli = parse_args(&args(&[
            "--opt-stats",
            "--opt-stats-json",
            "compile",
            "a.hy",
        ]))
        .unwrap();
        assert!(cli.opt_stats && cli.opt_stats_json);
        assert!(parse_args(&args(&["test", "--opt-stats"])).is_err());
        assert!(parse_args(&args(&["run", "out.hyc", "--opt-stats-json"])).is_err());
    }

    #[test]
    fn parse_pgo_flags() {
        let cli = parse_args(&args(&["--pgo-instrument", "compile", "a.hy"])).unwrap();
        assert!(cli.pgo_instrument);
        let cli = parse_args(&args(&["--pgo-use-profile=p.json", "a.hy"])).unwrap();
        assert_eq!(cli.pgo_use_profile.as_deref(), Some("p.json"));
        let cli = parse_args(&args(&[
            "--pgo-generate-profile",
            "out.json",
            "compile",
            "a.hy",
        ]))
        .unwrap();
        assert_eq!(cli.pgo_generate_profile.as_deref(), Some("out.json"));
        assert!(parse_args(&args(&["test", "--pgo-instrument"])).is_err());
    }
}
