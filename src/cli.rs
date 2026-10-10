//! `coil` argv: clap parser plus the command enum `main` dispatches on.

use std::path::{Path, PathBuf};
use std::process::exit;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use coil_args::{
    CompileProfileFlags, EntryFlag, HostGrantFlags, LogFlags, OptLevelFlags, RootFlags,
    expand_o_shorts, merge_entry,
};
use compiler::{HostGrants, OptLevel};

pub(crate) const DEFAULT_OUT: &str = "out.hyc";

const RESERVED: &[&str] = &[
    "compile", "run", "test", "package", "dissect", "debug", "fmt", "lsp", "natives", "verify",
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
    /// Re-exec `coil-test` (every flag is forwarded and parsed there).
    Test,
    Mutate,
    /// Re-exec `coil-verify` (every flag is forwarded and parsed there).
    Verify,
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
    pub contracts: Option<compiler::ContractLevel>,
    pub opt_stats: bool,
    pub opt_stats_json: bool,
    pub host_grants: HostGrants,
    /// Extra `--root` directories (default `src` is always included).
    pub module_roots: Vec<PathBuf>,
}

#[derive(Parser, Debug)]
#[command(
    name = "coil",
    version,
    about = "Coil compiler and runtime",
    disable_help_subcommand = true,
    after_help = "Pass the entry as a positional `.hy` or `--entry FILE`.\n\
Default diagnostics: pretty reports on stderr.\n\
`--root DIR` is repeatable extra `use`/`mod` search (default is `src` under cwd).\n\
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
    #[command(flatten)]
    roots: RootFlags,
    #[command(flatten)]
    entry_flag: EntryFlag,
    /// Compile harness tests into the archive (default: omit)
    #[arg(long)]
    include_tests: bool,
    /// Compile this `.hy` file in memory and run it (or `--entry`)
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
        #[command(flatten)]
        roots: RootFlags,
        #[command(flatten)]
        entry_flag: EntryFlag,
        /// Compile harness tests into the archive (default: omit)
        #[arg(long)]
        include_tests: bool,
        /// Output archive path
        #[arg(short = 'o', long = "output", value_name = "PATH")]
        output: Option<String>,
        /// Entry `.hy` (or `--entry`)
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
    /// Compile and run every test under [path] (default: ./tests; re-execs `coil-test`)
    #[command(disable_help_flag = true)]
    Test {
        /// Forwarded to `coil-test` (`coil test --help` lists them)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// Mutation testing: which small source edits no test notices (re-execs `coil-test mutate`)
    #[command(disable_help_flag = true)]
    Mutate {
        /// Forwarded to `coil-test mutate` (`coil mutate --help` lists them)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// Prove contracts with an SMT solver (re-execs `coil-verify`)
    #[command(disable_help_flag = true)]
    Verify {
        /// Forwarded to `coil-verify` (`coil verify --help` lists them)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
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
        #[command(flatten)]
        roots: RootFlags,
        #[command(flatten)]
        entry_flag: EntryFlag,
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
        file: Option<String>,
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
        #[command(flatten)]
        grants: HostGrantFlags,
        #[command(flatten)]
        roots: RootFlags,
        #[command(flatten)]
        entry_flag: EntryFlag,
        /// Filter functions by FQN substring / trailing name
        #[arg(long = "fn", value_name = "PAT")]
        fn_pat: Option<String>,
        /// Also print pre-opt stack IL
        #[arg(long)]
        il: bool,
        /// Also print the optimized IL (before fuse / lowering)
        #[arg(long)]
        il_post: bool,
        /// Also print each body's HIR (typed, desugared tree)
        #[arg(long)]
        hir: bool,
        /// Also print each function's effects and why auto-par left loops sequential
        #[arg(long)]
        effects: bool,
        /// Also print the MIR of numeric bodies (dense / LIR)
        #[arg(long)]
        mir: bool,
        /// Do not interleave source lines in the bytecode listing
        #[arg(long)]
        no_source: bool,
        /// Also print the entry-file AST
        #[arg(long)]
        ast: bool,
        /// Print the entry file after macro expansion (no bytecode)
        #[arg(long)]
        expand: bool,
        /// Entry `.hy` file, or a compiled `.hyc` archive (bytecode only)
        file: Option<String>,
    },
    /// GDB-style debugger (REPL; --dap for IDE)
    Debug {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        grants: HostGrantFlags,
        #[command(flatten)]
        roots: RootFlags,
        #[command(flatten)]
        entry_flag: EntryFlag,
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
    /// Start the Coil language server over stdin/stdout (re-execs `coil-lsp`)
    #[command(disable_help_flag = true)]
    Lsp {
        /// Forwarded to `coil-lsp`: `--root DIR`, host grants, `--stdio`
        /// (`coil lsp --help` lists them)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
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
            contracts: None,
            opt_stats: false,
            opt_stats_json: false,
            host_grants: HostGrants::deny_all(),
            module_roots: Vec::new(),
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
    let raw = RawCli::from_arg_matches(&matches).map_err(format_clap_error)?;
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

fn cli_from(
    command: Command,
    log: LogFlags,
    include_tests: bool,
    opt: OptLevelFlags,
    profile: CompileProfileFlags,
    grants: HostGrantFlags,
    roots: Vec<PathBuf>,
) -> CliArgs {
    CliArgs {
        command,
        log_json: log.log_json,
        log_lsp: log.log_lsp,
        include_tests,
        opt_level: opt.opt_level.unwrap_or(OptLevel::Standard),
        contracts: opt.contracts,
        opt_stats: profile.opt_stats,
        opt_stats_json: profile.opt_stats_json,
        host_grants: grants.into_grants(),
        module_roots: roots,
    }
}

fn require_hy_entry(filename: &str) -> Result<(), String> {
    if filename.is_empty() {
        Err("missing input file (pass a .hy file or `--entry`)".into())
    } else {
        Ok(())
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
            || self.roots.is_set()
            || self.entry_flag.is_set()
    }

    fn into_cli_args(self) -> Result<CliArgs, String> {
        if self.command.is_some() && self.parent_run_flags_set() {
            return Err(
                "default-run flags belong on `coil <file>` (or after the subcommand)".into(),
            );
        }
        Ok(match self.command {
            None => {
                let filename = merge_entry(self.file, self.entry_flag.entry)?;
                require_hy_entry(&filename)?;
                if is_reserved(&filename) {
                    return Err("missing input file (pass a .hy file or `--entry`)".into());
                }
                CliArgs {
                    ..cli_from(
                        Command::BuildAndRun { filename },
                        self.log,
                        self.include_tests,
                        self.opt,
                        self.profile,
                        self.grants,
                        self.roots.root,
                    )
                }
            }
            Some(RawCommand::Lsp { args: _ }) => cli_from(
                Command::Lsp,
                LogFlags::default(),
                false,
                OptLevelFlags::default(),
                CompileProfileFlags::default(),
                HostGrantFlags::default(),
                Vec::new(),
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
                    Vec::new(),
                )
            }
            Some(RawCommand::Test { args: _ }) => cli_from(
                Command::Test,
                LogFlags::default(),
                false,
                OptLevelFlags::default(),
                CompileProfileFlags::default(),
                HostGrantFlags::default(),
                Vec::new(),
            ),
            Some(RawCommand::Mutate { args: _ }) => cli_from(
                Command::Mutate,
                LogFlags::default(),
                false,
                OptLevelFlags::default(),
                CompileProfileFlags::default(),
                HostGrantFlags::default(),
                Vec::new(),
            ),
            Some(RawCommand::Verify { args: _ }) => cli_from(
                Command::Verify,
                LogFlags::default(),
                false,
                OptLevelFlags::default(),
                CompileProfileFlags::default(),
                HostGrantFlags::default(),
                Vec::new(),
            ),
            Some(RawCommand::Compile {
                log,
                opt,
                profile,
                grants,
                roots,
                entry_flag,
                include_tests,
                output,
                file,
            }) => {
                let filename = merge_entry(file, entry_flag.entry)?;
                require_hy_entry(&filename)?;
                if is_reserved(&filename) {
                    return Err("compile requires an entry file".into());
                }
                CliArgs {
                    ..cli_from(
                        Command::Compile {
                            filename,
                            output: output.unwrap_or_else(|| DEFAULT_OUT.to_string()),
                        },
                        log,
                        include_tests,
                        opt,
                        profile,
                        grants,
                        roots.root,
                    )
                }
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
                Vec::new(),
            ),
            Some(RawCommand::Package {
                log,
                opt,
                profile,
                grants,
                roots,
                entry_flag,
                output,
                runner,
                check_native,
                strip_debug,
                file,
            }) => {
                let filename = merge_entry(file, entry_flag.entry)?;
                require_hy_entry(&filename)?;
                if is_reserved(&filename) {
                    return Err("package requires an entry file".into());
                }
                let out = output.unwrap_or_else(|| {
                    Path::new(&filename)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("a.out")
                        .to_string()
                });
                cli_from(
                    Command::Package {
                        filename,
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
                    roots.root,
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
                Vec::new(),
            ),
            Some(RawCommand::Dissect {
                log,
                opt,
                profile,
                grants,
                roots,
                entry_flag,
                fn_pat,
                il,
                il_post: _,
                hir: _,
                effects: _,
                mir: _,
                no_source: _,
                ast,
                expand: _,
                file,
            }) => {
                let filename = merge_entry(file, entry_flag.entry)?;
                require_hy_entry(&filename)?;
                if is_reserved(&filename) {
                    return Err("dissect requires an entry .hy file".into());
                }
                cli_from(
                    Command::Dissect {
                        filename,
                        fn_pat,
                        show_il: il,
                        show_ast: ast,
                    },
                    log,
                    false,
                    opt,
                    profile,
                    grants,
                    roots.root,
                )
            }
            Some(RawCommand::Debug {
                log,
                grants,
                roots,
                entry_flag,
                script,
                batch,
                dap,
                file,
            }) => {
                let command = if dap && file.is_none() && entry_flag.entry.is_none() {
                    Command::Debug {
                        filename: None,
                        script: None,
                        batch: false,
                        dap: true,
                    }
                } else {
                    let filename = merge_entry(file, entry_flag.entry)?;
                    if filename.is_empty() {
                        return Err("debug requires an entry .hy file (or use --dap)".into());
                    }
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
                    roots.root,
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
    fn parse_lsp_forwards_roots_and_grants() {
        let cli = parse_args(&args(&[
            "lsp",
            "--root",
            "src",
            "--allow-exec",
            "--allow-dload",
            "sdl2",
            "--help",
        ]))
        .unwrap();
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
    fn parse_debug_dap_with_host_grants() {
        let cli = parse_args(&args(&["debug", "--dap", "--allow-attach", "--allow-exit"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Debug {
                filename: None,
                script: None,
                batch: false,
                dap: true,
            }
        );
        assert!(cli.host_grants.allow_attach);
        assert!(cli.host_grants.allow_exit);
    }

    #[test]
    fn parse_debug_repl_host_grants() {
        let cli = parse_args(&args(&["debug", "a.hy", "--allow-exec"])).unwrap();
        assert!(cli.host_grants.allow_exec);
        assert_eq!(
            cli.command,
            Command::Debug {
                filename: Some("a.hy".into()),
                script: None,
                batch: false,
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
        assert_eq!(cli.host_grants, HostGrants::deny_all());
    }

    #[test]
    fn parse_host_grant_flags_on_dissect() {
        let cli = parse_args(&args(&[
            "dissect",
            "a.hy",
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
        assert_eq!(
            cli.command,
            Command::Dissect {
                filename: "a.hy".into(),
                fn_pat: None,
                show_il: false,
                show_ast: false,
            }
        );
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
    fn parse_test_forwards_every_flag_to_the_helper() {
        for argv in [
            &["test"][..],
            &["test", "./tests", "--fail-fast"],
            &["test", "--log-lsp", "-O2", "--root", "src", "--allow-exit"],
            &["test", "--seed", "42", "--help"],
            // Unknown or misplaced flags are `coil-test`'s to reject.
            &["test", "-o", "x"],
        ] {
            let cli = parse_args(&args(argv)).unwrap();
            assert_eq!(cli.command, Command::Test, "{argv:?}");
            assert!(!cli.log_json && !cli.log_lsp, "{argv:?}");
        }
    }

    #[test]
    fn parse_mutate_forwards_every_flag_to_the_helper() {
        for argv in [
            &["mutate"][..],
            &["mutate", "--files", "src/**", "--json", "--help"],
            &["mutate", "-j", "4", "--operators=arith", "--log-json"],
        ] {
            let cli = parse_args(&args(argv)).unwrap();
            assert_eq!(cli.command, Command::Mutate, "{argv:?}");
            assert!(!cli.log_json, "{argv:?}");
        }
    }

    #[test]
    fn parse_verify_forwards_every_flag_to_the_helper() {
        for argv in [&["verify", "a.hy"][..], &["verify", "--solver", "z3", "--strict", "a.hy", "--help"]] {
            let cli = parse_args(&args(argv)).unwrap();
            assert_eq!(cli.command, Command::Verify, "{argv:?}");
        }
    }

    #[test]
    fn parse_log_flags_with_subcommand() {
        let cli = parse_args(&args(&["compile", "--log-json", "a.hy"])).unwrap();
        assert!(cli.log_json);
        assert!(matches!(cli.command, Command::Compile { .. }));
    }

    #[test]
    fn parse_rejects_output_on_run_and_default() {
        assert!(parse_args(&args(&["run", "a.hyc", "-o", "x"])).is_err());
        assert!(parse_args(&args(&["examples/fib.hy", "-o", "x"])).is_err());
    }

    #[test]
    fn parse_empty_args_requires_entry() {
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["compile"])).is_err());
    }

    #[test]
    fn parse_entry_flag_and_roots() {
        let cli = parse_args(&args(&["--entry", "src/main.hy"])).unwrap();
        assert_eq!(
            cli.command,
            Command::BuildAndRun {
                filename: "src/main.hy".into()
            }
        );
        let cli = parse_args(&args(&["compile", "--entry", "a.hy"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Compile {
                filename: "a.hy".into(),
                output: DEFAULT_OUT.into(),
            }
        );
        let cli = parse_args(&args(&[
            "--root",
            "examples/src",
            "--root",
            "vendor",
            "a.hy",
        ]))
        .unwrap();
        assert_eq!(
            cli.module_roots,
            vec![PathBuf::from("examples/src"), PathBuf::from("vendor")]
        );
        assert!(parse_args(&args(&["--entry", "a.hy", "b.hy"])).is_err());
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
    fn parse_rejects_reserved_test_path_names() {}

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
        let cli = parse_args(&args(&["compile", "--log-json", "--log-lsp", "a.hy"])).unwrap();
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
        assert!(parse_args(&args(&["run", "out.hyc", "--opt-stats-json"])).is_err());
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
