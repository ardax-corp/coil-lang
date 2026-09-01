//! `coil` argv: clap parser plus the command enum `main` dispatches on.

use std::path::{Path, PathBuf};
use std::process::exit;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use compiler::{HostGrants, OptLevel};

pub(crate) const DEFAULT_OUT: &str = "out.hyc";

const RESERVED: &[&str] = &[
    "compile", "run", "test", "package", "dissect", "debug", "fmt", "lsp", "natives",
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
    /// Dump / list native lock metadata for `spool download`.
    Natives {
        /// Packaged executable (omit to use project `[[ffi.native]]`).
        exe: Option<String>,
        /// Emit fetch TSV instead of JSON.
        tsv: bool,
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
    pub host_grants: HostGrants,
}

/// SARIF / LSP diagnostic stream (commands that report through the compiler).
#[derive(Args, Clone, Debug, Default)]
struct LogFlags {
    /// Emit SARIF 2.1 diagnostics on stdout
    #[arg(long)]
    log_json: bool,
    /// Emit LSP Diagnostic NDJSON on stdout
    #[arg(long)]
    log_lsp: bool,
}

/// `-O` / `--opt-level` for commands that compile Coil source.
#[derive(Args, Clone, Debug, Default)]
struct OptLevelFlags {
    /// none/0, basic/1, standard/2 (default), aggressive/3, size/s, debug/g
    #[arg(short = 'O', long = "opt-level", value_name = "LEVEL", value_parser = parse_opt_level)]
    opt_level: Option<OptLevel>,
}

/// Opt-stat dump and PGO (need a compile, not `run` / `test` / `debug`).
#[derive(Args, Clone, Debug, Default)]
struct CompileProfileFlags {
    /// Print IL optimization counters after compile (stderr)
    #[arg(long)]
    opt_stats: bool,
    /// Print the same counters as one JSON object (stderr)
    #[arg(long)]
    opt_stats_json: bool,
    /// Insert pgo_hit counters at function/block/branch sites
    #[arg(long)]
    pgo_instrument: bool,
    /// Apply a JSON profile to layout and inlining
    #[arg(long, value_name = "FILE")]
    pgo_use_profile: Option<String>,
    /// Write runtime or current profile JSON
    #[arg(long, value_name = "FILE")]
    pgo_generate_profile: Option<String>,
}

/// Host capabilities. Default deny (same as a missing coil.toml).
///
/// Not read from Manifest. Used for **compile/typecheck** (`E0406`–`E0411`).
/// `coil run out.hyc` and coil-embed do not re-apply these flags; the artifact
/// is the grant. `--ffi-search-path` is lookup, not a dload grant.
/// `dload("c")` stays denied even with `--allow-dload c`.
#[derive(Args, Clone, Debug, Default)]
struct HostGrantFlags {
    /// Allow Stream.attach
    #[arg(long)]
    allow_attach: bool,
    /// Allow env::exit
    #[arg(long)]
    allow_exit: bool,
    /// Allow env::exec
    #[arg(long)]
    allow_exec: bool,
    /// Allow FFI process-exec symbols (system, execve, …)
    #[arg(long)]
    allow_ffi_exec: bool,
    /// Allow dload of STEM (repeatable). Still needs lock hash or trusted.
    #[arg(long = "allow-dload", value_name = "STEM", action = clap::ArgAction::Append)]
    allow_dload: Vec<String>,
    /// Extra FFI library search directory (repeatable; lookup only)
    #[arg(long = "ffi-search-path", value_name = "DIR", action = clap::ArgAction::Append)]
    ffi_search_path: Vec<PathBuf>,
}

impl HostGrantFlags {
    fn is_set(&self) -> bool {
        self.allow_attach
            || self.allow_exit
            || self.allow_exec
            || self.allow_ffi_exec
            || !self.allow_dload.is_empty()
            || !self.ffi_search_path.is_empty()
    }

    fn into_grants(self) -> HostGrants {
        HostGrants {
            allow_attach: self.allow_attach,
            allow_exec: self.allow_exec,
            allow_exit: self.allow_exit,
            allow_ffi_exec: self.allow_ffi_exec,
            allow_dload: self.allow_dload,
            ffi_search_paths: self.ffi_search_path,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "coil",
    version,
    about = "Coil compiler and runtime",
    disable_help_subcommand = true,
    after_help = "When no file is given, `coil` / `coil compile` use `[entry].file` from coil.toml.\n\
Default diagnostics: pretty reports on stderr.\n\
Host grants (`--allow-attach`, `--allow-exec`, `--allow-exit`, `--allow-ffi-exec`,\n\
`--allow-dload STEM`) are CLI / Pipeline API for compile and typecheck — coil.toml\n\
does not grant them. `coil run out.hyc` and coil-embed do not re-apply allow flags;\n\
if the bytecode has the op, it runs. `--ffi-search-path` is lookup only.\n\
`dload(\"c\")` stays denied even if flagged."
)]
struct RawCli {
    #[command(flatten)]
    log: LogFlags,
    #[command(flatten)]
    opt: OptLevelFlags,
    #[command(flatten)]
    profile: CompileProfileFlags,
    #[command(flatten)]
    grants: HostGrantFlags,
    /// Compile harness tests into the archive (default: omit)
    #[arg(long)]
    include_tests: bool,
    /// Compile this `.hy` file in memory and run it (or `[entry].file` from coil.toml)
    #[arg(value_name = "FILE")]
    file: Option<String>,
    #[command(subcommand)]
    command: Option<RawCommand>,
}

#[derive(Subcommand, Debug)]
enum RawCommand {
    /// Compile an entry file (must define main) to a .hyc archive
    Compile {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        opt: OptLevelFlags,
        #[command(flatten)]
        profile: CompileProfileFlags,
        #[command(flatten)]
        grants: HostGrantFlags,
        /// Compile harness tests into the archive (default: omit)
        #[arg(long)]
        include_tests: bool,
        /// Output archive path
        #[arg(short = 'o', long = "output", value_name = "PATH")]
        output: Option<String>,
        /// Entry `.hy` (or `[entry].file` from coil.toml)
        file: Option<String>,
    },
    /// Execute a previously compiled .hyc archive
    Run {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        grants: HostGrantFlags,
        /// Archive path
        archive: String,
    },
    /// Compile and run every .hy file under [path] (default: ./tests)
    Test {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        opt: OptLevelFlags,
        #[command(flatten)]
        grants: HostGrantFlags,
        /// Stop after the first failed case
        #[arg(long)]
        fail_fast: bool,
        /// Test root (files under `compile_fail/` must be rejected)
        path: Option<String>,
    },
    /// Build a single-host executable (runner + embedded .hyc)
    Package {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        opt: OptLevelFlags,
        #[command(flatten)]
        profile: CompileProfileFlags,
        #[command(flatten)]
        grants: HostGrantFlags,
        /// Packaged binary path (default: entry file stem)
        #[arg(short = 'o', long = "output", value_name = "PATH")]
        output: Option<String>,
        /// Runner template (default: `coil-embed` beside this binary)
        #[arg(long, value_name = "PATH")]
        runner: Option<PathBuf>,
        /// Fail if required shared libraries are missing
        #[arg(long)]
        check_native: bool,
        /// Omit debug line table from the embedded archive
        #[arg(long)]
        strip_debug: bool,
        /// Entry `.hy` file
        file: String,
    },
    /// Native lock helpers for `spool download`
    Natives {
        #[command(subcommand)]
        action: NativesAction,
    },
    /// In-memory compile and dump filtered bytecode / IL / AST
    Dissect {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        opt: OptLevelFlags,
        #[command(flatten)]
        profile: CompileProfileFlags,
        /// Filter functions by FQN substring / trailing name
        #[arg(long = "fn", value_name = "PAT")]
        fn_pat: Option<String>,
        /// Also print pre-opt stack IL
        #[arg(long)]
        il: bool,
        /// Also print the entry-file AST
        #[arg(long)]
        ast: bool,
        /// Entry `.hy` file
        file: String,
    },
    /// GDB-style debugger (REPL; --dap for IDE)
    Debug {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        grants: HostGrantFlags,
        /// Run commands from a script file
        #[arg(short = 'x', value_name = "SCRIPT")]
        script: Option<String>,
        /// Non-interactive (use -x or stdin); exit after script
        #[arg(long)]
        batch: bool,
        /// Debug Adapter Protocol over stdio
        #[arg(long)]
        dap: bool,
        /// Entry `.hy` file (omit with `--dap`)
        file: Option<String>,
    },
    /// Format `.hy` sources (re-execs `coil-fmt`)
    Fmt {
        /// Exit 1 if files would change (no writes)
        #[arg(long)]
        check: bool,
        /// Files or directories
        #[arg(required = true, trailing_var_arg = true)]
        paths: Vec<String>,
    },
    /// Start the Coil language server over stdin/stdout
    Lsp {
        /// Accepted for LSP clients; ignored
        #[arg(long, hide = true)]
        stdio: bool,
    },
}

#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
enum NativesAction {
    /// Print the native lock (JSON by default) for a packaged exe or the current project
    Dump {
        /// Packaged executable; omit to read `[[ffi.native]]` from the project `coil.toml`
        file: Option<String>,
        /// Emit fetch TSV: package, version, filename, url, sha256, size
        #[arg(long)]
        tsv: bool,
    },
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
            host_grants: HostGrants::deny_all(),
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
        format!("{line} (see `coil --help` or `-V`/`--version`)")
    } else {
        line.to_string()
    }
}

impl LogFlags {
    fn is_set(&self) -> bool {
        self.log_json || self.log_lsp
    }
}

impl OptLevelFlags {
    fn is_set(&self) -> bool {
        self.opt_level.is_some()
    }
}

impl CompileProfileFlags {
    fn is_set(&self) -> bool {
        self.opt_stats
            || self.opt_stats_json
            || self.pgo_instrument
            || self.pgo_use_profile.is_some()
            || self.pgo_generate_profile.is_some()
    }
}

fn cli_from(
    command: Command,
    log: LogFlags,
    include_tests: bool,
    opt: OptLevelFlags,
    profile: CompileProfileFlags,
    grants: HostGrantFlags,
) -> CliArgs {
    CliArgs {
        command,
        log_json: log.log_json,
        log_lsp: log.log_lsp,
        include_tests,
        opt_level: opt.opt_level.unwrap_or(OptLevel::Standard),
        opt_stats: profile.opt_stats,
        opt_stats_json: profile.opt_stats_json,
        pgo_instrument: profile.pgo_instrument,
        pgo_use_profile: profile.pgo_use_profile,
        pgo_generate_profile: profile.pgo_generate_profile,
        host_grants: grants.into_grants(),
    }
}

impl RawCli {
    fn parent_run_flags_set(&self) -> bool {
        self.log.is_set()
            || self.opt.is_set()
            || self.profile.is_set()
            || self.include_tests
            || self.file.is_some()
            || self.grants.is_set()
    }

    fn into_cli_args(self) -> Result<CliArgs, String> {
        if self.command.is_some() && self.parent_run_flags_set() {
            return Err(
                "default-run flags belong on `coil <file>` (or after the subcommand)".into(),
            );
        }
        Ok(match self.command {
            None => cli_from(
                Command::BuildAndRun {
                    filename: self.file.unwrap_or_default(),
                },
                self.log,
                self.include_tests,
                self.opt,
                self.profile,
                self.grants,
            ),
            Some(RawCommand::Lsp { stdio: _ }) => cli_from(
                Command::Lsp,
                LogFlags::default(),
                false,
                OptLevelFlags::default(),
                CompileProfileFlags::default(),
                HostGrantFlags::default(),
            ),
            Some(RawCommand::Fmt { paths, check: _ }) => {
                if paths.is_empty() {
                    return Err("fmt requires at least one file or directory".into());
                }
                cli_from(
                    Command::Fmt,
                    LogFlags::default(),
                    false,
                    OptLevelFlags::default(),
                    CompileProfileFlags::default(),
                    HostGrantFlags::default(),
                )
            }
            Some(RawCommand::Test {
                log,
                opt,
                grants,
                fail_fast,
                path,
            }) => {
                if let Some(p) = &path {
                    if is_reserved(p) {
                        return Err("test path must be a directory".into());
                    }
                }
                cli_from(
                    Command::Test { path, fail_fast },
                    log,
                    false,
                    opt,
                    CompileProfileFlags::default(),
                    grants,
                )
            }
            Some(RawCommand::Compile {
                log,
                opt,
                profile,
                grants,
                include_tests,
                output,
                file,
            }) => {
                let filename = file.unwrap_or_default();
                if !filename.is_empty() && is_reserved(&filename) {
                    return Err("compile requires an entry file".into());
                }
                cli_from(
                    Command::Compile {
                        filename,
                        output: output.unwrap_or_else(|| DEFAULT_OUT.to_string()),
                    },
                    log,
                    include_tests,
                    opt,
                    profile,
                    grants,
                )
            }
            Some(RawCommand::Run {
                log,
                grants,
                archive,
            }) => cli_from(
                Command::Run { archive },
                log,
                false,
                OptLevelFlags::default(),
                CompileProfileFlags::default(),
                grants,
            ),
            Some(RawCommand::Package {
                log,
                opt,
                profile,
                grants,
                output,
                runner,
                check_native,
                strip_debug,
                file,
            }) => {
                if is_reserved(&file) {
                    return Err("package requires an entry file".into());
                }
                let out = output.unwrap_or_else(|| {
                    Path::new(&file)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("a.out")
                        .to_string()
                });
                cli_from(
                    Command::Package {
                        filename: file,
                        output: out,
                        runner,
                        check_native,
                        strip_debug,
                    },
                    log,
                    false,
                    opt,
                    profile,
                    grants,
                )
            }
            Some(RawCommand::Natives {
                action: NativesAction::Dump { file, tsv },
            }) => cli_from(
                Command::Natives { exe: file, tsv },
                LogFlags::default(),
                false,
                OptLevelFlags::default(),
                CompileProfileFlags::default(),
                HostGrantFlags::default(),
            ),
            Some(RawCommand::Dissect {
                log,
                opt,
                profile,
                fn_pat,
                il,
                ast,
                file,
            }) => {
                if is_reserved(&file) {
                    return Err("dissect requires an entry .hy file".into());
                }
                cli_from(
                    Command::Dissect {
                        filename: file,
                        fn_pat,
                        show_il: il,
                        show_ast: ast,
                    },
                    log,
                    false,
                    opt,
                    profile,
                    HostGrantFlags::default(),
                )
            }
            Some(RawCommand::Debug {
                log,
                grants,
                script,
                batch,
                dap,
                file,
            }) => {
                let command = if dap && file.is_none() {
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
                    if dap {
                        return Err("--dap cannot be combined with a positional .hy file".into());
                    }
                    Command::Debug {
                        filename: Some(filename),
                        script,
                        batch,
                        dap,
                    }
                };
                cli_from(
                    command,
                    log,
                    false,
                    OptLevelFlags::default(),
                    CompileProfileFlags::default(),
                    grants,
                )
            }
        })
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
        let cli = parse_args(&args(&["lsp", "--stdio"])).unwrap();
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
        let cli = parse_args(&args(&["compile", "--output", "x.hyc", "a.hy"])).unwrap();
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
        let cli = parse_args(&args(&["test", "--fail-fast"])).unwrap();
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
        let cli = parse_args(&args(&["compile", "--log-json", "a.hy"])).unwrap();
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
        let cli = parse_args(&args(&["test", "--log-json", "--log-lsp"])).unwrap();
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
        let cli = parse_args(&args(&["compile", "--opt-level", "aggressive", "a.hy"])).unwrap();
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
        let cli = parse_args(&args(&["compile", "--opt-stats", "a.hy"])).unwrap();
        assert!(cli.opt_stats && !cli.opt_stats_json);
        let cli = parse_args(&args(&["--opt-stats-json", "a.hy"])).unwrap();
        assert!(cli.opt_stats_json && !cli.opt_stats);
        let cli = parse_args(&args(&[
            "compile",
            "--opt-stats",
            "--opt-stats-json",
            "a.hy",
        ]))
        .unwrap();
        assert!(cli.opt_stats && cli.opt_stats_json);
        assert!(parse_args(&args(&["test", "--opt-stats"])).is_err());
        assert!(parse_args(&args(&["run", "out.hyc", "--opt-stats-json"])).is_err());
    }

    #[test]
    fn parse_pgo_flags() {
        let cli = parse_args(&args(&["compile", "--pgo-instrument", "a.hy"])).unwrap();
        assert!(cli.pgo_instrument);
        let cli = parse_args(&args(&["--pgo-use-profile=p.json", "a.hy"])).unwrap();
        assert_eq!(cli.pgo_use_profile.as_deref(), Some("p.json"));
        let cli = parse_args(&args(&[
            "compile",
            "--pgo-generate-profile",
            "out.json",
            "a.hy",
        ]))
        .unwrap();
        assert_eq!(cli.pgo_generate_profile.as_deref(), Some("out.json"));
        assert!(parse_args(&args(&["test", "--pgo-instrument"])).is_err());
    }

    #[test]
    fn parse_host_grant_flags_on_run_and_default() {
        let cli = parse_args(&args(&[
            "run",
            "out.hyc",
            "--allow-attach",
            "--allow-exec",
            "--allow-exit",
            "--allow-ffi-exec",
            "--allow-dload",
            "tls",
            "--allow-dload",
            "crypto",
            "--ffi-search-path",
            "./native",
        ]))
        .unwrap();
        assert!(cli.host_grants.allow_attach);
        assert!(cli.host_grants.allow_exec);
        assert!(cli.host_grants.allow_exit);
        assert!(cli.host_grants.allow_ffi_exec);
        assert_eq!(
            cli.host_grants.allow_dload,
            vec!["tls".to_string(), "crypto".to_string()]
        );
        assert_eq!(
            cli.host_grants.ffi_search_paths,
            vec![PathBuf::from("./native")]
        );

        let cli = parse_args(&args(&["--allow-exec", "examples/fib.hy"])).unwrap();
        assert!(cli.host_grants.allow_exec);
        assert!(!cli.host_grants.allow_attach);

        let cli = parse_args(&args(&["run", "out.hyc"])).unwrap();
        assert_eq!(cli.host_grants, HostGrants::deny_all());

        let cli = parse_args(&args(&["compile", "--allow-dload", "c", "a.hy"])).unwrap();
        assert_eq!(cli.host_grants.allow_dload, vec!["c".to_string()]);
    }

    #[test]
    fn parse_rejects_parent_grant_flags_with_subcommand() {
        assert!(parse_args(&args(&["--allow-exec", "compile", "a.hy"])).is_err());
    }
}
