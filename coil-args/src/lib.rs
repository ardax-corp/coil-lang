//! Shared clap flag groups for `coil` and the helper binaries it re-execs.
//!
//! Host grants, module roots, opt level, and log format are spelled once so
//! `coil compile` and `coil-test` / `coil-debug` / `coil-dissect` / `coil-lsp`
//! accept the same tokens. `coil-embed` does not depend on this crate.

use std::path::PathBuf;

use clap::{ArgMatches, Args, Command, FromArgMatches};
use compiler::{ComptimeGrants, ComptimeLimits, ContractLevel, DebugLevel, HostGrants, OptLevel, Scoped};

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

/// `-g0` / `-g1` / `-g2` for commands that compile Coil source
/// (coil-lang#1491). `--strip-debug` is `-g0`.
#[derive(Args, Clone, Debug, Default)]
pub struct DebugLevelFlags {
    /// Debug info: 0 none, 1 function names and lines, 2 lines and byte
    /// spans (default 2; 1 at -Os and for `coil package`)
    #[arg(short = 'g', value_name = "LEVEL", value_parser = DebugLevel::parse)]
    pub debug_level: Option<DebugLevel>,
    /// Omit all debug info (line table, function names, source paths): -g0
    #[arg(long, conflicts_with = "debug_level")]
    pub strip_debug: bool,
}

impl DebugLevelFlags {
    pub fn is_set(&self) -> bool {
        self.debug_level.is_some() || self.strip_debug
    }

    /// The level asked for, if any (`None`: the command's default).
    pub fn level(&self) -> Option<DebugLevel> {
        if self.strip_debug {
            Some(DebugLevel::None)
        } else {
            self.debug_level
        }
    }
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

/// Host capabilities and `dload` integrity. Default deny.
///
/// Capabilities are checked at **compile** (`E0406`–`E0411`, `E0414`).
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
    /// Allow dload of STEM (repeatable). Still needs --dload-pin or --dload-trusted.
    #[arg(long = "allow-dload", value_name = "STEM", action = clap::ArgAction::Append)]
    pub allow_dload: Vec<String>,
    /// Extra FFI library search directory (repeatable; lookup only)
    #[arg(long = "ffi-search-path", value_name = "DIR", action = clap::ArgAction::Append)]
    pub ffi_search_path: Vec<PathBuf>,
    /// The library dload loads for STEM must have this SHA-256 (repeatable)
    #[arg(
        long = "dload-pin",
        value_name = "STEM=SHA256",
        value_parser = parse_dload_pin,
        action = clap::ArgAction::Append
    )]
    pub dload_pin: Vec<(String, String)>,
    /// Load STEM's library without a hash check (repeatable)
    #[arg(long = "dload-trusted", value_name = "STEM", action = clap::ArgAction::Append)]
    pub dload_trusted: Vec<String>,
    /// Most steps (loop iterations + calls) one compile-time evaluation may
    /// run, such as a macro call (default 1000000000)
    #[arg(long = "comptime-steps", value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub comptime_steps: Option<u64>,
    /// Most memory, in MiB, one compile-time evaluation may keep live
    /// (default 1024)
    #[arg(long = "comptime-mem", value_name = "MIB", value_parser = clap::value_parser!(u64).range(1..=1 << 40))]
    pub comptime_mem: Option<u64>,
    /// Let compile-time code read files under PATH; `MODULE=PATH` for code in
    /// MODULE and the modules under it only (repeatable)
    #[arg(long = "comptime-read", value_name = "[MODULE=]PATH", action = clap::ArgAction::Append)]
    pub comptime_read: Vec<String>,
    /// Let compile-time code write files under PATH (repeatable, `MODULE=` as above)
    #[arg(long = "comptime-write", value_name = "[MODULE=]PATH", action = clap::ArgAction::Append)]
    pub comptime_write: Vec<String>,
    /// Let compile-time code read environment variable NAME (repeatable,
    /// `MODULE=` as above)
    #[arg(long = "comptime-env", value_name = "[MODULE=]NAME", action = clap::ArgAction::Append)]
    pub comptime_env: Vec<String>,
    /// Let compile-time code run programs named STEM (repeatable, `MODULE=`
    /// as above)
    #[arg(long = "comptime-exec", value_name = "[MODULE=]STEM", action = clap::ArgAction::Append)]
    pub comptime_exec: Vec<String>,
    /// Let compile-time code connect to HOST (any port) or HOST:PORT over
    /// TCP (repeatable, `MODULE=` as above)
    #[arg(long = "comptime-net", value_name = "[MODULE=]HOST[:PORT]", action = clap::ArgAction::Append)]
    pub comptime_net: Vec<String>,
    /// Most seconds one compile-time evaluation may take, checked at each
    /// IO call; one connect or read waits at most 30 (default 300)
    #[arg(long = "comptime-timeout", value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
    pub comptime_timeout: Option<u64>,
    /// Keep the results of compile-time IO in DIR (default
    /// ~/.cache/coil/comptime)
    #[arg(long = "comptime-cache", value_name = "DIR")]
    pub comptime_cache: Option<PathBuf>,
    /// Use cached compile-time results only: a result that is not cached
    /// may not read the environment or run a program
    #[arg(long = "comptime-offline", conflicts_with = "comptime_refresh")]
    pub comptime_offline: bool,
    /// Compute cached compile-time results that used the environment or a
    /// program again, and report which changed
    #[arg(long = "comptime-refresh")]
    pub comptime_refresh: bool,
}

/// Parse grant and compile-time limit flags from `words` (a test file's
/// `// Flags:` header), as the command line would.
pub fn parse_grant_flags(words: &[&str]) -> Result<HostGrantFlags, String> {
    #[derive(clap::Parser)]
    #[command(no_binary_name = true, disable_help_flag = true)]
    struct Words {
        #[command(flatten)]
        grants: HostGrantFlags,
    }
    <Words as clap::Parser>::try_parse_from(words)
        .map(|w| w.grants)
        .map_err(|e| e.render().to_string().lines().next().unwrap_or("bad flags").to_string())
}

/// `STEM=SHA256` with a 64-digit hex digest.
pub fn parse_dload_pin(s: &str) -> Result<(String, String), String> {
    let (stem, hash) = s
        .split_once('=')
        .ok_or_else(|| format!("expected STEM=SHA256, got `{s}`"))?;
    if stem.is_empty() {
        return Err(format!("missing stem in `{s}`"));
    }
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("`{hash}` is not a 64-digit hex SHA-256"));
    }
    Ok((stem.to_string(), hash.to_ascii_lowercase()))
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
            || !self.dload_pin.is_empty()
            || !self.dload_trusted.is_empty()
            || self.comptime_steps.is_some()
            || self.comptime_mem.is_some()
            || !self.comptime_read.is_empty()
            || !self.comptime_write.is_empty()
            || !self.comptime_env.is_empty()
            || !self.comptime_exec.is_empty()
            || !self.comptime_net.is_empty()
            || self.comptime_timeout.is_some()
            || self.comptime_cache.is_some()
            || self.comptime_offline
            || self.comptime_refresh
    }

    /// The `--comptime-read` / `-write` / `-env` / `-exec` grants.
    fn comptime_io(&self) -> ComptimeGrants {
        let paths = |list: &[String]| list.iter().map(|g| Scoped::parse(g, |p| PathBuf::from(p))).collect();
        let names = |list: &[String]| list.iter().map(|g| Scoped::parse(g, |n| n.to_string())).collect();
        ComptimeGrants {
            read: paths(&self.comptime_read),
            write: paths(&self.comptime_write),
            env: names(&self.comptime_env),
            exec: names(&self.comptime_exec),
            net: names(&self.comptime_net),
        }
    }

    /// Add these flags to `grants`: capabilities and lists add up, and a
    /// limit given here replaces the one in `grants`.
    pub fn apply_to(self, grants: &mut HostGrants) {
        let limits = grants.comptime;
        let steps = self.comptime_steps;
        let mem = self.comptime_mem;
        let timeout = self.comptime_timeout;
        let extra = self.into_grants();
        grants.comptime_io.extend(extra.comptime_io);
        if extra.comptime_cache.dir.is_some() {
            grants.comptime_cache.dir = extra.comptime_cache.dir;
        }
        grants.comptime_cache.offline |= extra.comptime_cache.offline;
        grants.comptime_cache.refresh |= extra.comptime_cache.refresh;
        grants.allow_attach |= extra.allow_attach;
        grants.allow_exec |= extra.allow_exec;
        grants.allow_exit |= extra.allow_exit;
        grants.allow_ffi_exec |= extra.allow_ffi_exec;
        grants.allow_read |= extra.allow_read;
        grants.allow_write |= extra.allow_write;
        grants.allow_net |= extra.allow_net;
        grants.allow_env |= extra.allow_env;
        for stem in extra.allow_dload {
            grants.grant_dload_allow(stem);
        }
        for path in extra.ffi_search_paths {
            grants.add_ffi_search_path(path);
        }
        grants.dload_pins.extend(extra.dload_pins);
        grants.dload_trusted.extend(extra.dload_trusted);
        grants.comptime = ComptimeLimits {
            steps: steps.unwrap_or(limits.steps),
            heap_bytes: mem.map_or(limits.heap_bytes, |m| m << 20),
            timeout_secs: timeout.unwrap_or(limits.timeout_secs),
        };
    }

    pub fn into_grants(self) -> HostGrants {
        let comptime_io = self.comptime_io();
        let mut grants = HostGrants {
            comptime_io,
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
            dload_pins: self.dload_pin,
            dload_trusted: self.dload_trusted,
            comptime: ComptimeLimits {
                steps: self.comptime_steps.unwrap_or(ComptimeLimits::DEFAULT_STEPS),
                heap_bytes: self.comptime_mem.unwrap_or(ComptimeLimits::DEFAULT_MEM_MIB) << 20,
                timeout_secs: self.comptime_timeout.unwrap_or(ComptimeLimits::DEFAULT_TIMEOUT_SECS),
            },
            comptime_cache: compiler::comptime_cache::CacheConfig {
                dir: self.comptime_cache,
                offline: self.comptime_offline,
                refresh: self.comptime_refresh,
                replay: false,
            },
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
