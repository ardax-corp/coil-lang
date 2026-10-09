//! Shared clap flag groups for `coil` and the helper binaries it re-execs.
//!
//! Host grants, module roots, opt level, and log format are spelled once so
//! `coil compile` and `coil-test` / `coil-debug` / `coil-dissect` / `coil-lsp`
//! accept the same tokens. `coil-embed` does not depend on this crate.

use std::path::PathBuf;

use clap::{ArgMatches, Args, Command, FromArgMatches};
use compiler::{ContractLevel, HostGrants, OptLevel};

/// SARIF / LSP diagnostic stream (commands that report through the compiler).
#[derive(Args, Clone, Debug, Default)]
pub struct LogFlags {
    /// Emit SARIF 2.1 diagnostics on stdout
    #[arg(long)]
    pub log_json: bool,
    /// Emit LSP Diagnostic NDJSON on stdout
    #[arg(long)]
    pub log_lsp: bool,
}

impl LogFlags {
    pub fn is_set(&self) -> bool {
        self.log_json || self.log_lsp
    }
}

/// `-O` / `--opt-level` for commands that compile Coil source.
#[derive(Args, Clone, Debug, Default)]
pub struct OptLevelFlags {
    /// none/0, basic/1, standard/2 (default), aggressive/3, size/s, debug/g
    #[arg(short = 'O', long = "opt-level", value_name = "LEVEL", value_parser = parse_opt_level)]
    pub opt_level: Option<OptLevel>,
    /// Contract checks: all, requires, off (default: all at -O0/-O1/-Og and
    /// in `coil test`, requires at -O2 and above)
    #[arg(long, value_name = "LEVEL", value_parser = ContractLevel::parse)]
    pub contracts: Option<ContractLevel>,
}

impl OptLevelFlags {
    pub fn is_set(&self) -> bool {
        self.opt_level.is_some()
    }

    pub fn level(&self) -> OptLevel {
        self.opt_level.unwrap_or(OptLevel::Standard)
    }
}

fn parse_opt_level(s: &str) -> Result<OptLevel, String> {
    OptLevel::parse(s).map_err(|_| {
        "invalid --opt-level (expected none|basic|standard|aggressive|size|debug or 0|1|2|3|s|g)"
            .into()
    })
}

/// Opt-stat dump (need a compile, not `run` / `test` / `debug`).
#[derive(Args, Clone, Debug, Default)]
pub struct CompileProfileFlags {
    /// Print IL optimization counters after compile (stderr)
    #[arg(long)]
    pub opt_stats: bool,
    /// Print the same counters as one JSON object (stderr)
    #[arg(long)]
    pub opt_stats_json: bool,
}

impl CompileProfileFlags {
    pub fn is_set(&self) -> bool {
        self.opt_stats || self.opt_stats_json
    }
}

/// Host capabilities. Default deny (same as a missing coil.toml).
///
/// Not read from Manifest. Used at **compile** (`E0406`–`E0411`, `E0414`).
/// `coil run out.hyc` and coil-embed do not re-apply these flags; the artifact
/// is the grant. `--ffi-search-path` is lookup, not a dload grant.
/// `dload("c")` stays denied even with `--allow-dload c`.
#[derive(Args, Clone, Debug, Default)]
pub struct HostGrantFlags {
    /// Allow Stream.attach
    #[arg(long)]
    pub allow_attach: bool,
    /// Allow env::exit
    #[arg(long)]
    pub allow_exit: bool,
    /// Allow env::exec
    #[arg(long)]
    pub allow_exec: bool,
    /// Allow FFI process-exec symbols (system, execve, …)
    #[arg(long)]
    pub allow_ffi_exec: bool,
    /// Allow opening files for reading and inspecting the file system
    #[arg(long)]
    pub allow_read: bool,
    /// Allow opening files for writing, creating, removing and renaming
    #[arg(long)]
    pub allow_write: bool,
    /// Allow connecting, listening on and binding sockets
    #[arg(long)]
    pub allow_net: bool,
    /// Allow reading and changing environment variables and the working directory
    #[arg(long)]
    pub allow_env: bool,
    /// Allow everything above (not dload)
    #[arg(short = 'A', long)]
    pub allow_all: bool,
    /// Allow dload of STEM (repeatable). Still needs lock hash or trusted.
    #[arg(long = "allow-dload", value_name = "STEM", action = clap::ArgAction::Append)]
    pub allow_dload: Vec<String>,
    /// Extra FFI library search directory (repeatable; lookup only)
    #[arg(long = "ffi-search-path", value_name = "DIR", action = clap::ArgAction::Append)]
    pub ffi_search_path: Vec<PathBuf>,
}

impl HostGrantFlags {
    pub fn is_set(&self) -> bool {
        self.allow_attach
            || self.allow_exit
            || self.allow_exec
            || self.allow_ffi_exec
            || self.allow_read
            || self.allow_write
            || self.allow_net
            || self.allow_env
            || self.allow_all
            || !self.allow_dload.is_empty()
            || !self.ffi_search_path.is_empty()
    }

    pub fn into_grants(self) -> HostGrants {
        let mut grants = HostGrants {
            allow_attach: self.allow_attach,
            allow_exec: self.allow_exec,
            allow_exit: self.allow_exit,
            allow_ffi_exec: self.allow_ffi_exec,
            allow_read: self.allow_read,
            allow_write: self.allow_write,
            allow_net: self.allow_net,
            allow_env: self.allow_env,
            allow_dload: self.allow_dload,
            ffi_search_paths: self.ffi_search_path,
        };
        if self.allow_all {
            grants.grant_all();
        }
        grants
    }
}

/// Extra `use`/`mod` search directories (`--root`, repeatable).
#[derive(Args, Clone, Debug, Default)]
pub struct RootFlags {
    /// Extra module search directory (repeatable). Default is `src` under cwd.
    #[arg(long = "root", value_name = "DIR", action = clap::ArgAction::Append)]
    pub root: Vec<PathBuf>,
}

impl RootFlags {
    pub fn is_set(&self) -> bool {
        !self.root.is_empty()
    }
}

/// `--entry` as an alternative to a positional `.hy` file.
#[derive(Args, Clone, Debug, Default)]
pub struct EntryFlag {
    /// Entry `.hy` (instead of the positional file)
    #[arg(long = "entry", value_name = "FILE")]
    pub entry: Option<String>,
}

impl EntryFlag {
    pub fn is_set(&self) -> bool {
        self.entry.is_some()
    }
}

/// Positional file and `--entry`, or an empty string when both are absent.
///
/// The same path passed both ways is one entry. Two different paths is an error.
pub fn merge_entry(positional: Option<String>, flag: Option<String>) -> Result<String, String> {
    let pos = positional.filter(|s| !s.is_empty());
    let flag = flag.filter(|s| !s.is_empty());
    match (pos, flag) {
        (Some(a), Some(b)) if a != b => {
            Err("pass the entry as a positional file or `--entry`, not both".into())
        }
        (Some(a), _) => Ok(a),
        (_, Some(b)) => Ok(b),
        (None, None) => Ok(String::new()),
    }
}

/// Split glued `-O2` / `-Og` into `-O` plus the level.
///
/// Dispatch forwards the original argv, so helpers see `-O2` even though
/// clap's `-O` takes a separate value. `--…` tokens are left alone.
pub fn expand_o_shorts(args: &[String]) -> Vec<String> {
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

/// Parse `args` with `command`. `Ok(None)` means the user asked for help.
pub fn parse_with<T: FromArgMatches>(
    command: Command,
    args: &[String],
) -> Result<Option<T>, String> {
    let matches = match command.try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error) if is_help(&error) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    from_matches(matches)
}

fn from_matches<T: FromArgMatches>(matches: ArgMatches) -> Result<Option<T>, String> {
    T::from_arg_matches(&matches)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn is_help(error: &clap::Error) -> bool {
    error.kind() == clap::error::ErrorKind::DisplayHelp
        || error.kind() == clap::error::ErrorKind::DisplayVersion
}

/// Write clap's long help for `command` (stdout, exit is the caller's).
pub fn print_command_help(mut command: Command, bin_name: &str) {
    command.set_bin_name(bin_name);
    let _ = command.print_long_help();
    println!();
}

/// Print a parse failure. Clap errors already end in a newline and name the usage.
pub fn print_cli_error(msg: &str) {
    if msg.ends_with('\n') {
        eprint!("{msg}");
    } else {
        eprintln!("{msg}");
    }
}
