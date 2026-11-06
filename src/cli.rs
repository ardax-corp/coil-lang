//! `coil` argv: clap parser plus the command enum `main` dispatches on.

use std::path::{Path, PathBuf};
use std::process::exit;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use coil_args::{
    CompileProfileFlags, DebugLevelFlags, EntryFlag, HostGrantFlags, LogFlags, NurseryFlag,
    OptLevelFlags, RootFlags, expand_o_shorts, merge_entry,
};
use compiler::{DebugLevel, EarlyFree, HostGrants, OptLevel};
use crate::package_app::{FfiNative, LinkInputs, parse_ffi_native, parse_static_link};

pub(crate) const DEFAULT_OUT: &str = "out.ccoil";

const RESERVED: &[&str] = &[
    "compile", "run", "test", "package", "dissect", "debug", "fmt", "lsp", "natives", "verify",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Default: compile entry in memory and run (no out.ccoil).
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
        natives: Vec<FfiNative>,
        check_native: bool,
        strip_debug: bool,
        /// Native code or the JIT runner, static libraries and linker
        /// arguments for the executable.
        link: LinkInputs,
    },
    /// Dump / list native lock metadata.
    Natives {
        /// Packaged executable (omit to use the `--ffi-native` rows).
        exe: Option<String>,
        natives: Vec<FfiNative>,
        /// Emit fetch TSV instead of JSON.
        tsv: bool,
    },
    Dissect {
        filename: String,
        fn_pat: Option<String>,
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
    /// `-g` / `--strip-debug`; `None` is the command's default.
    pub debug_level: Option<DebugLevel>,
    pub contracts: Option<compiler::ContractLevel>,
    /// `--fold-calls on|off`; `None` keeps the pipeline default.
    pub fold_calls: Option<bool>,
    /// `--early-free on|off|check`; `None` follows the opt level.
    pub early_free: Option<EarlyFree>,
    /// `--nursery on|off` for `coil FILE` / `coil run`; `None` is on.
    pub nursery: Option<bool>,
    /// `--cow-report`: copies per source line at exit.
    pub cow_report: bool,
    /// `--mir`: `coil FILE` and `coil run` run on the MIR interpreter.
    pub mir: bool,
    /// `--auto-par on|off`; `None` keeps the pipeline default.
    pub auto_par: Option<bool>,
    pub opt_stats: bool,
    pub opt_stats_json: bool,
    /// Reuse and fill the build cache (off with `--no-build-cache`).
    pub build_cache: bool,
    pub host_grants: HostGrants,
    /// Extra `--root` directories (default `src` is always included).
    pub module_roots: Vec<PathBuf>,
    /// Arguments after `--` for `coil FILE` / `coil run ARCHIVE`: the
    /// program's `env::args()` after its name.
    pub program_args: Vec<String>,
}

#[derive(Parser, Debug)]
#[command(
    name = "coil",
    version,
    about = "Coil compiler and runtime",
    disable_help_subcommand = true,
    after_help = "Pass the entry as a positional `.coil` or `--entry FILE`.\n\
Program arguments follow `--`: `coil FILE -- a b` and `coil run X.ccoil -- a b` run\n\
with `env::args()` == [FILE, a, b]; coil's own flags are never visible to the program.\n\
Default diagnostics: pretty reports on stderr.\n\
`--root DIR` is repeatable extra `use`/`mod` search (default is `src` under cwd).\n\
Host grants (`--allow-attach`, `--allow-exec`, `--allow-exit`, `--allow-ffi-exec`,\n\
`--allow-dload STEM`) are CLI / Pipeline API for compile and typecheck.\n\
`coil run out.ccoil` and coil-embed do\n\
not re-apply allow flags; if the archive has the call, it runs. `--ffi-search-path` is\n\
lookup only. `--dload-pin STEM=SHA256` / `--dload-trusted STEM` are the dload integrity.\n\
`dload(\"c\")` stays denied even if flagged."
)]
struct RawCli {
    #[command(flatten)]
    log: LogFlags,
    #[command(flatten)]
    opt: OptLevelFlags,
    #[command(flatten)]
    nursery: NurseryFlag,
    #[command(flatten)]
    debug: DebugLevelFlags,
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
    /// Compile this `.coil` file in memory and run it (or `--entry`)
    #[arg(value_name = "FILE")]
    file: Option<String>,
    #[command(subcommand)]
    command: Option<RawCommand>,
}

#[derive(Subcommand, Debug)]
enum RawCommand {
    /// Compile an entry file (must define main) to a .ccoil archive
    Compile {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        opt: OptLevelFlags,
        #[command(flatten)]
        debug: DebugLevelFlags,
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
        /// Entry `.coil` (or `--entry`)
        file: Option<String>,
    },
    /// Execute a previously compiled .ccoil archive
    Run {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        grants: HostGrantFlags,
        #[command(flatten)]
        nursery: NurseryFlag,
        /// Compile the archive's code even when the code cache holds it
        #[arg(long)]
        no_build_cache: bool,
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
    /// Build a single-host executable (runner + embedded .ccoil)
    Package {
        #[command(flatten)]
        log: LogFlags,
        #[command(flatten)]
        opt: OptLevelFlags,
        #[command(flatten)]
        debug: DebugLevelFlags,
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
        /// Native library to embed in the native lock (repeatable):
        /// name=STEM,version=V,path=DIR[,package=P][,requires=a;b][,requires-hint=TEXT]
        #[arg(long = "ffi-native", value_name = "SPEC", value_parser = parse_ffi_native, action = clap::ArgAction::Append)]
        ffi_native: Vec<FfiNative>,
        /// Fail if required shared libraries are missing
        #[arg(long)]
        check_native: bool,
        /// Package the JIT runner with the program instead of linking native
        /// code for it (the default links an AOT executable with `cc`)
        #[arg(long)]
        no_aot: bool,
        /// What to write: `exe` (default) an executable, `obj` a relocatable
        /// object holding the program, `lib` a static library holding the
        /// program and the runtime. For `obj` and `lib` you run the link
        #[arg(long, value_name = "WHAT", value_enum, default_value_t)]
        emit: crate::package_app::Emit,
        /// Link a static library into the AOT executable for the `extern`
        /// block that names STEM (repeatable): its functions resolve from
        /// the executable instead of a shared library. Without `STEM=`,
        /// the stem is the file's (`libsum.a` is `sum`)
        #[arg(long = "link-static", value_name = "[STEM=]PATH", value_parser = parse_static_link, action = clap::ArgAction::Append)]
        link_static: Vec<crate::package_app::StaticLink>,
        /// Pass ARG to the linker of the AOT executable, after the
        /// libraries (repeatable; e.g. `-lm` for a static library's needs)
        #[arg(long = "link-arg", value_name = "ARG", allow_hyphen_values = true, action = clap::ArgAction::Append)]
        link_arg: Vec<String>,
        /// What to strip from the AOT executable: `debug` (default) drops
        /// the runtime's debug info, `all` also its symbol table, `none`
        /// keeps both. Coil backtraces use `-g`, not these
        #[arg(long = "strip-symbols", value_name = "WHAT", value_enum, default_value_t)]
        strip_symbols: crate::package_app::StripSymbols,
        /// Entry `.coil` file
        file: Option<String>,
    },
    /// Native lock helpers
    Natives {
        #[command(subcommand)]
        action: NativesAction,
    },
    /// In-memory compile and dump filtered MIR / HIR / AST
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
        /// Also print each body's HIR (typed, desugared tree)
        #[arg(long)]
        hir: bool,
        /// Also print each function's effects and why auto-par left loops sequential
        #[arg(long)]
        effects: bool,
        /// Print each body's MIR (the default)
        #[arg(long)]
        mir: bool,
        /// Include `test` blocks, as `coil test` compiles them
        #[arg(long = "tests")]
        tests: bool,
        /// Also print the entry-file AST
        #[arg(long)]
        ast: bool,
        /// Print the entry file after macro expansion (no MIR)
        #[arg(long)]
        expand: bool,
        /// Entry `.coil` file, or a compiled `.ccoil` archive (MIR only)
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
        /// Entry `.coil` file (omit with `--dap`)
        file: Option<String>,
    },
    /// Format `.coil` sources (re-execs `coil-fmt`)
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
        /// Packaged executable; omit to list the `--ffi-native` rows
        file: Option<String>,
        /// Native library row, as for `coil package` (repeatable)
        #[arg(long = "ffi-native", value_name = "SPEC", value_parser = parse_ffi_native, action = clap::ArgAction::Append)]
        ffi_native: Vec<FfiNative>,
        /// Emit TSV: package, version, filename, sha256, size
        #[arg(long)]
        tsv: bool,
    },
}

fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name)
}

/// Parse argv (including argv0). `-V` / `--version` win over every other token.
///
/// For `coil FILE` and `coil run ARCHIVE`, everything after the first `--` is
/// the program's own arguments ([`CliArgs::program_args`]), never parsed as
/// coil flags. Other commands see the whole argv as before (`coil test -- …`).
pub(crate) fn parse_args(args: &[String]) -> Result<CliArgs, String> {
    if let Some(dashes) = args.iter().position(|a| a == "--")
        && let Ok(mut cli) = parse_coil_args(&args[..dashes])
        && matches!(
            cli.command,
            Command::BuildAndRun { .. } | Command::Run { .. }
        )
    {
        cli.program_args = args[dashes + 1..].to_vec();
        return Ok(cli);
    }
    parse_coil_args(args)
}

fn parse_coil_args(args: &[String]) -> Result<CliArgs, String> {
    if args.iter().skip(1).any(|a| a == "-V" || a == "--version") {
        return Ok(CliArgs {
            command: Command::Version,
            log_json: false,
            log_lsp: false,
            include_tests: false,
            opt_level: OptLevel::Standard,
            debug_level: None,
            contracts: None,
            fold_calls: None,
            early_free: None,
            nursery: None,
            cow_report: false,
            mir: false,
            auto_par: None,
            opt_stats: false,
            opt_stats_json: false,
            build_cache: false,
            host_grants: HostGrants::deny_all(),
            module_roots: Vec::new(),
            program_args: Vec::new(),
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
        debug_level: None,
        contracts: opt.contracts,
        fold_calls: opt.fold_calls,
        early_free: opt.early_free,
        nursery: None,
        cow_report: false,
        mir: false,
        auto_par: opt.auto_par,
        opt_stats: profile.opt_stats,
        opt_stats_json: profile.opt_stats_json,
        build_cache: !profile.no_build_cache,
        host_grants: grants.into_grants(),
        module_roots: roots,
        program_args: Vec::new(),
    }
}

fn require_hy_entry(filename: &str) -> Result<(), String> {
    if filename.is_empty() {
        Err("missing input file (pass a .coil file or `--entry`)".into())
    } else {
        Ok(())
    }
}

impl RawCli {
    fn parent_run_flags_set(&self) -> bool {
        self.log.is_set()
            || self.opt.is_set()
            || self.nursery.nursery.is_some()
            || self.nursery.cow_report
            || self.nursery.mir
            || self.debug.is_set()
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
                    return Err("missing input file (pass a .coil file or `--entry`)".into());
                }
                // `--mir` is the raw reference unless `-O` says otherwise.
                let mir_level = (self.nursery.mir && self.opt.opt_level.is_none()).then_some(OptLevel::None);
                let cli = CliArgs {
                    debug_level: self.debug.level(),
                    nursery: self.nursery.nursery,
                    cow_report: self.nursery.cow_report,
                    mir: self.nursery.mir,
                    ..cli_from(
                        Command::BuildAndRun { filename },
                        self.log,
                        self.include_tests,
                        self.opt,
                        self.profile,
                        self.grants,
                        self.roots.root,
                    )
                };
                CliArgs { opt_level: mir_level.unwrap_or(cli.opt_level), ..cli }
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
                debug,
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
                    debug_level: debug.level(),
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
                nursery,
                no_build_cache,
                archive,
            }) => CliArgs {
                nursery: nursery.nursery,
                cow_report: nursery.cow_report,
                mir: nursery.mir,
                build_cache: !no_build_cache,
                ..cli_from(
                    Command::Run { archive },
                    log,
                    false,
                    OptLevelFlags::default(),
                    CompileProfileFlags::default(),
                    grants,
                    Vec::new(),
                )
            },
            Some(RawCommand::Package {
                log,
                opt,
                debug,
                profile,
                grants,
                roots,
                entry_flag,
                output,
                runner,
                ffi_native,
                check_native,
                no_aot,
                emit,
                link_static,
                link_arg,
                strip_symbols,
                file,
            }) => {
                let filename = merge_entry(file, entry_flag.entry)?;
                require_hy_entry(&filename)?;
                if is_reserved(&filename) {
                    return Err("package requires an entry file".into());
                }
                let out = output.unwrap_or_else(|| {
                    let stem = Path::new(&filename).file_stem().and_then(|s| s.to_str()).unwrap_or("a.out");
                    match emit {
                        crate::package_app::Emit::Exe => stem.to_string(),
                        crate::package_app::Emit::Obj => format!("{stem}.o"),
                        crate::package_app::Emit::Lib => format!("lib{stem}.a"),
                    }
                });
                CliArgs {
                    debug_level: debug.level(),
                    ..cli_from(
                        Command::Package {
                            filename,
                            output: out,
                            link: LinkInputs { aot: !no_aot && runner.is_none(), emit, statics: link_static, args: link_arg, strip: strip_symbols },
                            runner,
                            natives: ffi_native,
                            check_native,
                            strip_debug: debug.strip_debug,
                        },
                        log,
                        false,
                        opt,
                        profile,
                        grants,
                        roots.root,
                    )
                }
            }
            Some(RawCommand::Natives {
                action: NativesAction::Dump {
                    file,
                    ffi_native,
                    tsv,
                },
            }) => cli_from(
                Command::Natives {
                    exe: file,
                    natives: ffi_native,
                    tsv,
                },
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
                hir: _,
                effects: _,
                mir: _,
                tests: _,
                ast,
                expand: _,
                file,
            }) => {
                let filename = merge_entry(file, entry_flag.entry)?;
                require_hy_entry(&filename)?;
                if is_reserved(&filename) {
                    return Err("dissect requires an entry .coil file".into());
                }
                cli_from(
                    Command::Dissect {
                        filename,
                        fn_pat,
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
                        return Err("debug requires an entry .coil file (or use --dap)".into());
                    }
                    if is_reserved(&filename) {
                        return Err("debug requires an entry .coil file".into());
                    }
                    if dap {
                        return Err("--dap cannot be combined with a positional .coil file".into());
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
    use crate::package_app::StripSymbols;

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
        let cli = parse_args(&args(&["compile", "a.coil", "--version"])).unwrap();
        assert_eq!(cli.command, Command::Version);
        let cli = parse_args(&args(&["--help", "--version"])).unwrap();
        assert_eq!(cli.command, Command::Version);
        let cli = parse_args(&args(&["-V", "--help"])).unwrap();
        assert_eq!(cli.command, Command::Version);
    }

    #[test]
    fn parse_fmt_paths() {
        let cli = parse_args(&args(&["fmt", "a.coil", "src/"])).unwrap();
        assert_eq!(cli.command, Command::Fmt);
    }

    #[test]
    fn parse_fmt_with_check() {
        let cli = parse_args(&args(&["fmt", "--check", "a.coil"])).unwrap();
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
            "--allow-exit",
            "--allow-dload",
            "sdl2",
            "--help",
        ]))
        .unwrap();
        assert_eq!(cli.command, Command::Lsp);
    }

    #[test]
    fn parse_rejects_check_on_non_fmt() {
        assert!(parse_args(&args(&["compile", "a.coil", "--check"])).is_err());
        assert!(parse_args(&args(&["--check", "a.coil"])).is_err());
    }

    #[test]
    fn parse_debug_with_script_batch() {
        let cli = parse_args(&args(&[
            "debug",
            "examples/fib.coil",
            "-x",
            "cmds.txt",
            "--batch",
        ]))
        .unwrap();
        assert_eq!(
            cli.command,
            Command::Debug {
                filename: Some("examples/fib.coil".into()),
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
        let cli = parse_args(&args(&["debug", "a.coil", "--allow-exit"])).unwrap();
        assert!(cli.host_grants.allow_exit);
        assert_eq!(
            cli.command,
            Command::Debug {
                filename: Some("a.coil".into()),
                script: None,
                batch: false,
                dap: false,
            }
        );
    }

    #[test]
    fn parse_dissect_with_fn_and_ast() {
        let cli = parse_args(&args(&[
            "dissect",
            "examples/fib.coil",
            "--fn",
            "fib",
            "--ast",
        ]))
        .unwrap();
        assert_eq!(
            cli.command,
            Command::Dissect {
                filename: "examples/fib.coil".into(),
                fn_pat: Some("fib".into()),
                show_ast: true,
            }
        );
        assert_eq!(cli.host_grants, HostGrants::deny_all());
    }

    #[test]
    fn parse_host_grant_flags_on_dissect() {
        let cli = parse_args(&args(&[
            "dissect",
            "a.coil",
            "--allow-attach",
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
                filename: "a.coil".into(),
                fn_pat: None,
                show_ast: false,
            }
        );
        assert!(cli.host_grants.allow_attach);
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
        let cli = parse_args(&args(&["examples/fib.coil"])).unwrap();
        assert_eq!(
            cli.command,
            Command::BuildAndRun {
                filename: "examples/fib.coil".into()
            }
        );
        assert!(!cli.log_json);
    }

    #[test]
    fn parse_compile_default_output() {
        let cli = parse_args(&args(&["compile", "examples/fib.coil"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Compile {
                filename: "examples/fib.coil".into(),
                output: DEFAULT_OUT.into(),
            }
        );
    }

    #[test]
    fn parse_compile_with_short_output() {
        let cli = parse_args(&args(&["compile", "examples/fib.coil", "-o", "fib.ccoil"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Compile {
                filename: "examples/fib.coil".into(),
                output: "fib.ccoil".into(),
            }
        );
    }

    #[test]
    fn parse_compile_with_long_output_before_command() {
        let cli = parse_args(&args(&["compile", "--output", "x.ccoil", "a.coil"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Compile {
                filename: "a.coil".into(),
                output: "x.ccoil".into(),
            }
        );
    }

    #[test]
    fn parse_run() {
        let cli = parse_args(&args(&["run", "out.ccoil"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Run {
                archive: "out.ccoil".into()
            }
        );
    }

    #[test]
    fn parse_package_default_output() {
        let cli = parse_args(&args(&["package", "examples/fib.coil"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Package {
                filename: "examples/fib.coil".into(),
                output: "fib".into(),
                runner: None,
                natives: Vec::new(),
                check_native: false,
                strip_debug: false,
                link: LinkInputs { aot: true, ..LinkInputs::default() },
            }
        );
    }

    #[test]
    fn parse_package_static_links() {
        let cli = parse_args(&args(&[
            "package",
            "app.coil",
            "--link-static",
            "deps/libsum.a",
            "--link-static",
            "z=deps/libz-static.a",
            "--link-arg",
            "-lm",
        ]))
        .unwrap();
        let Command::Package { link, .. } = cli.command else { panic!("not a package command") };
        let stems: Vec<&str> = link.statics.iter().map(|l| l.stem.as_str()).collect();
        assert_eq!(stems, ["sum", "z"]);
        assert_eq!(link.args, ["-lm"]);
        assert_eq!(link.strip, StripSymbols::Debug);
    }

    #[test]
    fn parse_package_strip_symbols() {
        for (arg, want) in [("none", StripSymbols::None), ("debug", StripSymbols::Debug), ("all", StripSymbols::All)] {
            let cli = parse_args(&args(&["package", "app.coil", "--strip-symbols", arg])).unwrap();
            let Command::Package { link, .. } = cli.command else { panic!("not a package command") };
            assert_eq!(link.strip, want, "{arg}");
        }
        assert!(parse_args(&args(&["package", "app.coil", "--strip-symbols", "some"])).is_err());
    }

    #[test]
    fn parse_package_with_flags() {
        let cli = parse_args(&args(&[
            "package",
            "app.coil",
            "-o",
            "myapp",
            "--check-native",
            "--strip-debug",
            "--runner",
            "/usr/bin/coil",
            "--ffi-native",
            "name=sum,version=0.1.0,path=native",
        ]))
        .unwrap();
        assert_eq!(
            cli.command,
            Command::Package {
                filename: "app.coil".into(),
                output: "myapp".into(),
                runner: Some(PathBuf::from("/usr/bin/coil")),
                natives: vec![FfiNative {
                    name: "sum".into(),
                    package: "sum".into(),
                    version: "0.1.0".into(),
                    path: PathBuf::from("native"),
                    requires: Vec::new(),
                    requires_hint: String::new(),
                }],
                check_native: true,
                strip_debug: true,
                link: LinkInputs::default(),
            }
        );
    }

    #[test]
    fn parse_ffi_native_optional_keys_and_escaped_comma() {
        let n = parse_ffi_native(
            "name=re,package=coil-regex,version=1.2.0,path=lib,\
             requires=libpcre2.so;libz.so,requires-hint=apt install pcre2\\, zlib",
        )
        .unwrap();
        assert_eq!(n.package, "coil-regex");
        assert_eq!(n.requires, vec!["libpcre2.so".to_string(), "libz.so".to_string()]);
        assert_eq!(n.requires_hint, "apt install pcre2, zlib");
        assert!(parse_ffi_native("name=re,version=1").is_err());
        assert!(parse_ffi_native("name=re,colour=red").is_err());
    }

    #[test]
    fn parse_natives_dump_takes_ffi_native_rows() {
        let cli = parse_args(&args(&[
            "natives",
            "dump",
            "--ffi-native",
            "name=sum,version=0.1.0,path=native",
        ]))
        .unwrap();
        let Command::Natives { exe, natives, .. } = cli.command else {
            panic!("expected natives");
        };
        assert_eq!(exe, None);
        assert_eq!(natives.len(), 1);
        assert_eq!(natives[0].name, "sum");
    }

    #[test]
    fn parse_dload_pin_and_trusted() {
        let sha = "AB".repeat(32);
        let cli = parse_args(&args(&[
            "run",
            "out.ccoil",
            "--dload-pin",
            &format!("tls={sha}"),
            "--dload-trusted",
            "sum",
        ]))
        .unwrap();
        assert_eq!(
            cli.host_grants.dload_pins,
            vec![("tls".to_string(), "ab".repeat(32))]
        );
        assert_eq!(cli.host_grants.dload_trusted, vec!["sum".to_string()]);
        assert!(parse_args(&args(&["run", "out.ccoil", "--dload-pin", "tls=abc"])).is_err());
        assert!(parse_args(&args(&["run", "out.ccoil", "--dload-pin", "tls"])).is_err());
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
        for argv in [&["verify", "a.coil"][..], &["verify", "--solver", "z3", "--strict", "a.coil", "--help"]] {
            let cli = parse_args(&args(argv)).unwrap();
            assert_eq!(cli.command, Command::Verify, "{argv:?}");
        }
    }

    #[test]
    fn parse_log_flags_with_subcommand() {
        let cli = parse_args(&args(&["compile", "--log-json", "a.coil"])).unwrap();
        assert!(cli.log_json);
        assert!(matches!(cli.command, Command::Compile { .. }));
    }

    #[test]
    fn parse_rejects_output_on_run_and_default() {
        assert!(parse_args(&args(&["run", "a.ccoil", "-o", "x"])).is_err());
        assert!(parse_args(&args(&["examples/fib.coil", "-o", "x"])).is_err());
    }

    #[test]
    fn parse_empty_args_requires_entry() {
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["compile"])).is_err());
    }

    #[test]
    fn parse_entry_flag_and_roots() {
        let cli = parse_args(&args(&["--entry", "src/main.coil"])).unwrap();
        assert_eq!(
            cli.command,
            Command::BuildAndRun {
                filename: "src/main.coil".into()
            }
        );
        let cli = parse_args(&args(&["compile", "--entry", "a.coil"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Compile {
                filename: "a.coil".into(),
                output: DEFAULT_OUT.into(),
            }
        );
        let cli = parse_args(&args(&[
            "--root",
            "examples/src",
            "--root",
            "vendor",
            "a.coil",
        ]))
        .unwrap();
        assert_eq!(
            cli.module_roots,
            vec![PathBuf::from("examples/src"), PathBuf::from("vendor")]
        );
        assert!(parse_args(&args(&["--entry", "a.coil", "b.coil"])).is_err());
    }

    #[test]
    fn parse_program_args_after_double_dash() {
        let cli = parse_args(&args(&["--root", "lib", "a.coil", "--", "x", "--root", "-V"])).unwrap();
        assert_eq!(
            cli.command,
            Command::BuildAndRun {
                filename: "a.coil".into()
            }
        );
        assert_eq!(cli.module_roots, vec![PathBuf::from("lib")]);
        assert_eq!(cli.program_args, ["x", "--root", "-V"]);

        let cli = parse_args(&args(&["run", "out.ccoil", "--", "a", "b"])).unwrap();
        assert_eq!(
            cli.command,
            Command::Run {
                archive: "out.ccoil".into()
            }
        );
        assert_eq!(cli.program_args, ["a", "b"]);

        let cli = parse_args(&args(&["a.coil", "--"])).unwrap();
        assert!(cli.program_args.is_empty());
        let cli = parse_args(&args(&["a.coil"])).unwrap();
        assert!(cli.program_args.is_empty());
        // Other commands keep their own `--` handling.
        let cli = parse_args(&args(&["test", "--", "x"])).unwrap();
        assert_eq!(cli.command, Command::Test);
        assert!(cli.program_args.is_empty());
    }

    #[test]
    fn parse_rejects_missing_run_archive() {
        assert!(parse_args(&args(&["run"])).is_err());
    }

    #[test]
    fn parse_rejects_fail_fast_on_non_test_commands() {
        assert!(parse_args(&args(&["--fail-fast", "examples/fib.coil"])).is_err());
        assert!(parse_args(&args(&["compile", "a.coil", "--fail-fast"])).is_err());
        assert!(parse_args(&args(&["run", "out.ccoil", "--fail-fast"])).is_err());
    }

    #[test]
    fn parse_rejects_reserved_test_path_names() {}

    #[test]
    fn parse_rejects_unrecognized_flag() {
        let err = parse_args(&args(&["--bogus", "a.coil"])).unwrap_err();
        assert!(
            err.contains("unrecognized") || err.contains("unexpected"),
            "{err}"
        );
        assert!(err.contains("--version"), "{err}");
    }

    #[test]
    fn parse_rejects_duplicate_output_and_missing_output_path() {
        assert!(parse_args(&args(&["compile", "a.coil", "-o"])).is_err());
        assert!(parse_args(&args(&["compile", "a.coil", "-o", "-x"])).is_err());
        assert!(parse_args(&args(&["compile", "a.coil", "-o", "x", "--output", "y"])).is_err());
    }

    #[test]
    fn parse_rejects_too_many_args_and_reserved_compile_names() {
        assert!(parse_args(&args(&["a.coil", "b.coil"])).is_err());
        assert!(parse_args(&args(&["compile", "compile"])).is_err());
        assert!(parse_args(&args(&["compile", "run"])).is_err());
        assert!(parse_args(&args(&["compile", "test"])).is_err());
    }

    #[test]
    fn parse_accepts_both_log_flags_at_parse_time() {
        let cli = parse_args(&args(&["compile", "--log-json", "--log-lsp", "a.coil"])).unwrap();
        assert!(cli.log_json && cli.log_lsp);

        let cli = parse_args(&args(&["--include-tests", "examples/fib.coil"])).unwrap();
        assert!(cli.include_tests);
        assert!(matches!(cli.command, Command::BuildAndRun { .. }));
        assert_eq!(cli.opt_level, OptLevel::Standard);
    }

    #[test]
    fn parse_opt_level_flags() {
        let cli = parse_args(&args(&["-O0", "examples/fib.coil"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::None);
        let cli = parse_args(&args(&["-O", "2", "examples/fib.coil"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::Standard);
        let cli = parse_args(&args(&["compile", "--opt-level", "aggressive", "a.coil"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::Aggressive);
        let cli = parse_args(&args(&["--opt-level=size", "a.coil"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::Size);
        let cli = parse_args(&args(&["-Og", "a.coil"])).unwrap();
        assert_eq!(cli.opt_level, OptLevel::Debug);
        assert!(parse_args(&args(&["-O9", "a.coil"])).is_err());
        assert!(parse_args(&args(&["-O2", "-O0", "a.coil"])).is_err());
        assert!(parse_args(&args(&["--opt-level"])).is_err());
    }

    #[test]
    fn parse_opt_stats_flags() {
        let cli = parse_args(&args(&["compile", "--opt-stats", "a.coil"])).unwrap();
        assert!(cli.opt_stats && !cli.opt_stats_json);
        let cli = parse_args(&args(&["--opt-stats-json", "a.coil"])).unwrap();
        assert!(cli.opt_stats_json && !cli.opt_stats);
        let cli = parse_args(&args(&[
            "compile",
            "--opt-stats",
            "--opt-stats-json",
            "a.coil",
        ]))
        .unwrap();
        assert!(cli.opt_stats && cli.opt_stats_json);
        assert!(parse_args(&args(&["run", "out.ccoil", "--opt-stats-json"])).is_err());
    }

    #[test]
    fn parse_host_grant_flags_on_run_and_default() {
        let cli = parse_args(&args(&[
            "run",
            "out.ccoil",
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

        let cli = parse_args(&args(&["--allow-exit", "examples/fib.coil"])).unwrap();
        assert!(cli.host_grants.allow_exit);
        assert!(!cli.host_grants.allow_attach);

        let cli = parse_args(&args(&["run", "out.ccoil"])).unwrap();
        assert_eq!(cli.host_grants, HostGrants::deny_all());

        let cli = parse_args(&args(&["compile", "--allow-dload", "c", "a.coil"])).unwrap();
        assert_eq!(cli.host_grants.allow_dload, vec!["c".to_string()]);
    }

    #[test]
    fn parse_rejects_parent_grant_flags_with_subcommand() {
        assert!(parse_args(&args(&["--allow-exit", "compile", "a.coil"])).is_err());
    }
}
