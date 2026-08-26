use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

use coil_cli::{LoadErr, dispatch_helper, try_load_archive};
use common::{ARCHIVE_VERSION, ArchivedProgram, Byte, ProgramDebug, format_archive_version};
use compiler::{OptLevel, Pipeline};
use machine::Machine;
use reporting::{ErrorCode, ReportConfig, ReportFormat};
use rkyv::rancor::Error;

const DEFAULT_OUT: &str = "out.hyc";
const TESTS_DIR: &str = "tests";

mod package_app;

use package_app::cmd_package;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
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
struct CliArgs {
    command: Command,
    log_json: bool,
    log_lsp: bool,
    include_tests: bool,
    opt_level: OptLevel,
    opt_stats: bool,
    opt_stats_json: bool,
    pgo_instrument: bool,
    pgo_use_profile: Option<String>,
    pgo_generate_profile: Option<String>,
}

fn print_help() {
    eprintln!(
        "Usage:\n\
         \x20 coil [--log-json | --log-lsp] [<file.hy>]\n\
         \x20 coil [--log-json | --log-lsp] compile [<file.hy>] [-o|--output <path>]\n\
         \x20 coil [--log-json | --log-lsp] run <file.hyc>\n\
         \x20 coil [--log-json | --log-lsp] package <file.hy> [-o|--output <path>]\n\
         \x20 coil [--log-json | --log-lsp] test [path] [--fail-fast]\n\
         \x20 coil [--log-json | --log-lsp] dissect <file.hy> [--fn <pat>] [--il] [--ast]\n\
         \x20 coil [--log-json | --log-lsp] debug [<file.hy> | --dap] [-x <script>] [--batch]\n\
         \x20 coil fmt [--check] <file.hy|dir>...\n\
         \x20 coil lsp\n\
         \n\
         Commands:\n\
         \x20 (default)  Compile <file.hy> (or `[entry].file` from coil.toml) in memory and run it\n\
         \x20 compile    Compile an entry file (must define main) to a .hyc archive\n\
         \x20 run        Execute a previously compiled .hyc archive\n\
         \x20 package    Build a single-host executable (runner + embedded .hyc)\n\
         \x20 test       Compile and run every .hy file under [path] (default: ./tests)\n\
         \x20             Files under a `compile_fail/` directory must be rejected with diagnostics\n\
         \x20 dissect    In-memory compile and dump filtered bytecode / IL / AST (no archive file)\n\
         \x20 debug      GDB-style debugger (REPL; --dap for IDE; optional -x script / --batch)\n\
         \x20 fmt        Format `.hy` sources (re-execs `coil-fmt`; preserves `//` and `///`)\n\
         \x20 lsp        Start the Coil language server over stdin/stdout\n\
         \n\
         Options:\n\
         \x20 -o, --output <path>  Output archive for `compile` or packaged binary for `package`\n\
         \x20 --runner <path>       Runner template for `package` (default: `coil-embed` beside this binary)\n\
         \x20 --check-native        With `package`, fail if required shared libraries are missing\n\
         \x20 --strip-debug         With `package`, omit debug line table from embedded archive\n\
         \x20 --include-tests      Compile harness tests into the archive (default: omit)\n\
         \x20 -O, --opt-level <l>  none/0, basic/1, standard/2 (default), aggressive/3, size/s, debug/g\n\
         \x20 --opt-stats          Print IL optimization counters after compile (stderr)\n\
         \x20 --opt-stats-json     Print the same counters as one JSON object (stderr)\n\
         \x20 --pgo-instrument     Insert pgo_hit counters at function/block/branch sites\n\
         \x20 --pgo-use-profile <f>  Apply a JSON profile to layout and inlining\n\
         \x20 --pgo-generate-profile <f>  Write runtime or current profile JSON\n\
         \x20 --fail-fast          With `test`, stop after the first failed case\n\
         \x20 --fn <pat>           With `dissect`, filter functions by FQN substring / trailing name\n\
         \x20 --il                 With `dissect`, also print pre-opt stack IL\n\
         \x20 --ast                With `dissect`, also print the entry-file AST\n\
         \x20 -x <script>          With `debug`, run commands from a script file\n\
         \x20 --batch              With `debug`, non-interactive (use -x or stdin); exit after script\n\
         \x20 --dap                With `debug`, Debug Adapter Protocol over stdio\n\
         \x20 --check              With `fmt`, exit 1 if files would change (no writes)\n\
         \x20 --log-json           Emit SARIF 2.1 diagnostics on stdout\n\
         \x20 --log-lsp            Emit LSP Diagnostic NDJSON on stdout\n\
         \x20 -h, --help           Show this help\n\
         \x20 -V, --version        Print the toolchain version and exit\n\
         \n\
         When no file is given, `coil` / `coil compile` use `[entry].file` from coil.toml.\n\
         (default diagnostics) Pretty reports on stderr"
    );
}

fn print_version() {
    println!("coil {}", env!("CARGO_PKG_VERSION"));
}

fn parse_args(args: &[String]) -> Result<CliArgs, &'static str> {
    // `-V` / `--version` win over every other token, including `-h` / `--help`.
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

    let mut log_json = false;
    let mut log_lsp = false;
    let mut fail_fast = false;
    let mut include_tests = false;
    let mut check_native = false;
    let mut strip_debug = false;
    let mut show_il = false;
    let mut show_ast = false;
    let mut batch = false;
    let mut dap = false;
    let mut check = false;
    let mut fn_pat: Option<String> = None;
    let mut script: Option<String> = None;
    let mut runner: Option<PathBuf> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut output: Option<String> = None;
    let mut opt_level = OptLevel::Standard;
    let mut opt_level_set = false;
    let mut opt_stats = false;
    let mut opt_stats_json = false;
    let mut pgo_instrument = false;
    let mut pgo_use_profile: Option<String> = None;
    let mut pgo_generate_profile: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--log-json" => log_json = true,
            "--log-lsp" => log_lsp = true,
            "--stdio" => {}
            "--fail-fast" => fail_fast = true,
            "--include-tests" => include_tests = true,
            "--opt-stats" => opt_stats = true,
            "--opt-stats-json" => opt_stats_json = true,
            "--pgo-instrument" => pgo_instrument = true,
            "--check-native" => check_native = true,
            "--strip-debug" => strip_debug = true,
            "--il" => show_il = true,
            "--ast" => show_ast = true,
            "--batch" => batch = true,
            "--dap" => dap = true,
            "--check" => check = true,
            "--fn" => {
                i += 1;
                let Some(pat) = args.get(i) else {
                    return Err("missing pattern after --fn");
                };
                if pat.starts_with('-') {
                    return Err("missing pattern after --fn");
                }
                if fn_pat.is_some() {
                    return Err("duplicate --fn flag");
                }
                fn_pat = Some(pat.clone());
            }
            "-x" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    return Err("missing path after -x");
                };
                if path.starts_with('-') {
                    return Err("missing path after -x");
                }
                if script.is_some() {
                    return Err("duplicate -x flag");
                }
                script = Some(path.clone());
            }
            "--runner" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    return Err("missing path after --runner");
                };
                if path.starts_with('-') {
                    return Err("missing path after --runner");
                }
                runner = Some(PathBuf::from(path));
            }
            "-h" | "--help" => {
                print_help();
                exit(0);
            }
            "-o" | "--output" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    return Err("missing path after -o/--output");
                };
                if path.starts_with('-') {
                    return Err("missing path after -o/--output");
                }
                if output.is_some() {
                    return Err("duplicate -o/--output flag");
                }
                output = Some(path.clone());
            }
            "--opt-level" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    return Err("missing value after --opt-level");
                };
                if val.starts_with('-') {
                    return Err("missing value after --opt-level");
                }
                if opt_level_set {
                    return Err("duplicate -O/--opt-level flag");
                }
                opt_level = OptLevel::parse(val).map_err(|_| "invalid --opt-level (expected none|basic|standard|aggressive|size|debug or 0|1|2|3|s|g)")?;
                opt_level_set = true;
            }
            s if s.starts_with("--opt-level=") => {
                let val = s.strip_prefix("--opt-level=").unwrap();
                if val.is_empty() {
                    return Err("missing value after --opt-level");
                }
                if opt_level_set {
                    return Err("duplicate -O/--opt-level flag");
                }
                opt_level = OptLevel::parse(val).map_err(|_| "invalid --opt-level (expected none|basic|standard|aggressive|size|debug or 0|1|2|3|s|g)")?;
                opt_level_set = true;
            }
            "-O" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    return Err("missing value after -O");
                };
                if val.starts_with('-') && OptLevel::parse(val.trim_start_matches('-')).is_err() {
                    return Err("missing value after -O");
                }
                let token = val.strip_prefix("-O").unwrap_or(val);
                if opt_level_set {
                    return Err("duplicate -O/--opt-level flag");
                }
                opt_level = OptLevel::parse(token).map_err(|_| "invalid -O level (expected 0|1|2|3|s|g or none|basic|standard|aggressive|size|debug)")?;
                opt_level_set = true;
            }
            s if s.starts_with("-O") && s.len() > 2 => {
                let token = &s[2..];
                if opt_level_set {
                    return Err("duplicate -O/--opt-level flag");
                }
                opt_level = OptLevel::parse(token).map_err(|_| "invalid -O level (expected 0|1|2|3|s|g or none|basic|standard|aggressive|size|debug)")?;
                opt_level_set = true;
            }
            "--pgo-use-profile" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    return Err("missing path after --pgo-use-profile");
                };
                if path.starts_with('-') {
                    return Err("missing path after --pgo-use-profile");
                }
                if pgo_use_profile.is_some() {
                    return Err("duplicate --pgo-use-profile flag");
                }
                pgo_use_profile = Some(path.clone());
            }
            s if s.starts_with("--pgo-use-profile=") => {
                let path = s.strip_prefix("--pgo-use-profile=").unwrap();
                if path.is_empty() {
                    return Err("missing path after --pgo-use-profile");
                }
                if pgo_use_profile.is_some() {
                    return Err("duplicate --pgo-use-profile flag");
                }
                pgo_use_profile = Some(path.to_string());
            }
            "--pgo-generate-profile" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    return Err("missing path after --pgo-generate-profile");
                };
                if path.starts_with('-') {
                    return Err("missing path after --pgo-generate-profile");
                }
                if pgo_generate_profile.is_some() {
                    return Err("duplicate --pgo-generate-profile flag");
                }
                pgo_generate_profile = Some(path.clone());
            }
            s if s.starts_with("--pgo-generate-profile=") => {
                let path = s.strip_prefix("--pgo-generate-profile=").unwrap();
                if path.is_empty() {
                    return Err("missing path after --pgo-generate-profile");
                }
                if pgo_generate_profile.is_some() {
                    return Err("duplicate --pgo-generate-profile flag");
                }
                pgo_generate_profile = Some(path.to_string());
            }
            s if s.starts_with('-') => {
                return Err(
                    "unrecognized flag (expected --log-json, --log-lsp, --stdio, --fail-fast, --include-tests, --opt-stats, --opt-stats-json, --pgo-instrument, --pgo-use-profile, --pgo-generate-profile, --check-native, --strip-debug, --runner, --fn, --il, --ast, -x, --batch, --check, -o/--output, -O/--opt-level, -V/--version, or a command/file)",
                );
            }
            _ => positionals.push(arg.clone()),
        }
        i += 1;
    }

    let has_dissect_flags = fn_pat.is_some() || show_il || show_ast;
    let has_debug_flags = script.is_some() || batch || dap;
    let has_fmt_flags = check;

    let command = match positionals.as_slice() {
        [cmd] if cmd == "lsp" => {
            if output.is_some()
                || fail_fast
                || include_tests
                || opt_stats
                || opt_stats_json
                || log_json
                || log_lsp
                || check_native
                || strip_debug
                || runner.is_some()
                || has_dissect_flags
                || has_debug_flags
                || has_fmt_flags
            {
                return Err("flags other than --stdio are not valid with `lsp`");
            }
            Command::Lsp
        }
        [cmd, rest @ ..] if cmd == "fmt" => {
            if rest.is_empty() {
                return Err("fmt requires at least one file or directory");
            }
            if output.is_some() {
                return Err("-o/--output is not valid with `fmt`");
            }
            if fail_fast {
                return Err("--fail-fast is only valid with `test`");
            }
            if include_tests {
                return Err("--include-tests is not valid with `fmt`");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            Command::Fmt
        }
        [] => {
            if output.is_some() {
                return Err("-o/--output is only valid with `compile` or `package`");
            }
            if fail_fast {
                return Err("--fail-fast is only valid with `test`");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            if has_fmt_flags {
                return Err("--check is only valid with `fmt`");
            }
            // Resolved later from coil.toml `[entry].file`.
            Command::BuildAndRun {
                filename: String::new(),
            }
        }
        [cmd] if cmd == "test" => {
            if output.is_some() {
                return Err("-o/--output is only valid with `compile` or `package`");
            }
            if include_tests {
                return Err("--include-tests is only valid with `compile` or the default run mode");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            if has_fmt_flags {
                return Err("--check is only valid with `fmt`");
            }
            Command::Test {
                path: None,
                fail_fast,
            }
        }
        [cmd, path] if cmd == "test" => {
            if output.is_some() {
                return Err("-o/--output is only valid with `compile` or `package`");
            }
            if include_tests {
                return Err("--include-tests is only valid with `compile` or the default run mode");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            if has_fmt_flags {
                return Err("--check is only valid with `fmt`");
            }
            if path == "compile"
                || path == "run"
                || path == "test"
                || path == "package"
                || path == "dissect"
                || path == "debug"
                || path == "fmt"
                || path == "lsp"
            {
                return Err("test path must be a directory");
            }
            Command::Test {
                path: Some(path.clone()),
                fail_fast,
            }
        }
        [cmd] if cmd == "compile" => {
            if fail_fast {
                return Err("--fail-fast is only valid with `test`");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            if has_fmt_flags {
                return Err("--check is only valid with `fmt`");
            }
            // Filename filled from `[entry].file` when empty.
            Command::Compile {
                filename: String::new(),
                output: output.unwrap_or_else(|| DEFAULT_OUT.to_string()),
            }
        }
        [cmd] if cmd == "run" => return Err("run requires a .hyc archive path"),
        [cmd] if cmd == "package" => return Err("package requires an entry file"),
        [cmd] if cmd == "dissect" => return Err("dissect requires an entry .hy file"),
        [cmd] if cmd == "debug" => {
            if dap {
                Command::Debug {
                    filename: None,
                    script: None,
                    batch: false,
                    dap: true,
                }
            } else {
                return Err("debug requires an entry .hy file (or use --dap)");
            }
        }
        [cmd, filename] if cmd == "package" => {
            if filename == "package"
                || filename == "compile"
                || filename == "run"
                || filename == "test"
                || filename == "dissect"
                || filename == "debug"
                || filename == "fmt"
                || filename == "lsp"
            {
                return Err("package requires an entry file");
            }
            if fail_fast {
                return Err("--fail-fast is only valid with `test`");
            }
            if include_tests {
                return Err("--include-tests is not valid with `package`");
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            if has_fmt_flags {
                return Err("--check is only valid with `fmt`");
            }
            let out = output.unwrap_or_else(|| {
                Path::new(filename)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("a.out")
                    .to_string()
            });
            Command::Package {
                filename: filename.clone(),
                output: out,
                runner,
                check_native,
                strip_debug,
            }
        }
        [cmd, filename] if cmd == "debug" => {
            if filename == "package"
                || filename == "compile"
                || filename == "run"
                || filename == "test"
                || filename == "dissect"
                || filename == "debug"
                || filename == "fmt"
                || filename == "lsp"
            {
                return Err("debug requires an entry .hy file");
            }
            if output.is_some() {
                return Err("-o/--output is not valid with `debug`");
            }
            if fail_fast {
                return Err("--fail-fast is only valid with `test`");
            }
            if include_tests {
                return Err("--include-tests is not valid with `debug`");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if dap {
                return Err("--dap cannot be combined with a positional .hy file");
            }
            Command::Debug {
                filename: Some(filename.clone()),
                script,
                batch,
                dap,
            }
        }
        [cmd, filename] if cmd == "dissect" => {
            if filename == "package"
                || filename == "compile"
                || filename == "run"
                || filename == "test"
                || filename == "dissect"
                || filename == "debug"
                || filename == "fmt"
                || filename == "lsp"
            {
                return Err("dissect requires an entry .hy file");
            }
            if output.is_some() {
                return Err("-o/--output is not valid with `dissect`");
            }
            if fail_fast {
                return Err("--fail-fast is only valid with `test`");
            }
            if include_tests {
                return Err("--include-tests is not valid with `dissect`");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            if has_fmt_flags {
                return Err("--check is only valid with `fmt`");
            }
            Command::Dissect {
                filename: filename.clone(),
                fn_pat,
                show_il,
                show_ast,
            }
        }
        [cmd, filename] if cmd == "compile" => {
            if filename == "compile"
                || filename == "run"
                || filename == "test"
                || filename == "package"
                || filename == "dissect"
                || filename == "debug"
                || filename == "fmt"
                || filename == "lsp"
            {
                return Err("compile requires an entry file");
            }
            if fail_fast {
                return Err("--fail-fast is only valid with `test`");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            if has_fmt_flags {
                return Err("--check is only valid with `fmt`");
            }
            Command::Compile {
                filename: filename.clone(),
                output: output.unwrap_or_else(|| DEFAULT_OUT.to_string()),
            }
        }
        [cmd, archive] if cmd == "run" => {
            if output.is_some() {
                return Err("-o/--output is only valid with `compile` or `package`");
            }
            if fail_fast {
                return Err("--fail-fast is only valid with `test`");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            if has_fmt_flags {
                return Err("--check is only valid with `fmt`");
            }
            Command::Run {
                archive: archive.clone(),
            }
        }
        [filename] => {
            if output.is_some() {
                return Err("-o/--output is only valid with `compile` or `package`");
            }
            if fail_fast {
                return Err("--fail-fast is only valid with `test`");
            }
            if check_native || strip_debug || runner.is_some() {
                return Err(
                    "--check-native, --strip-debug, and --runner are only valid with `package`",
                );
            }
            if has_dissect_flags {
                return Err("--fn, --il, and --ast are only valid with `dissect`");
            }
            if has_debug_flags {
                return Err("-x and --batch are only valid with `debug`");
            }
            if has_fmt_flags {
                return Err("--check is only valid with `fmt`");
            }
            Command::BuildAndRun {
                filename: filename.clone(),
            }
        }
        _ => return Err("too many arguments"),
    };

    if (opt_stats || opt_stats_json)
        && matches!(
            command,
            Command::Run { .. }
                | Command::Test { .. }
                | Command::Fmt
                | Command::Lsp
                | Command::Debug { .. }
        )
    {
        return Err("--opt-stats and --opt-stats-json require compiling source");
    }

    if (pgo_instrument || pgo_use_profile.is_some() || pgo_generate_profile.is_some())
        && matches!(
            command,
            Command::Run { .. }
                | Command::Test { .. }
                | Command::Fmt
                | Command::Lsp
                | Command::Debug { .. }
        )
    {
        return Err("PGO flags require compiling source");
    }

    Ok(CliArgs {
        command,
        log_json,
        log_lsp,
        include_tests,
        opt_level,
        opt_stats,
        opt_stats_json,
        pgo_instrument,
        pgo_use_profile,
        pgo_generate_profile,
    })
}

fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
    match format {
        ReportFormat::Pretty => Box::new(std::io::stderr()),
        ReportFormat::Sarif | ReportFormat::Lsp => Box::new(std::io::stdout()),
    }
}

pub(crate) fn fail_and_exit(
    pipeline: &mut Pipeline,
    code: ErrorCode,
    message: impl Into<String>,
) -> ! {
    pipeline.emit_spanless_error(code, message);
    let _ = pipeline.finish_reporting();
    exit(1);
}

fn resolve_entry_filename(pipeline: &mut Pipeline, filename: &str) -> String {
    if !filename.is_empty() {
        return filename.to_string();
    }
    match pipeline.manifest_entry_path() {
        Some(path) => {
            let display = path.display().to_string();
            if !path.exists() {
                fail_and_exit(
                    pipeline,
                    ErrorCode::MissingInputFile,
                    format!(
                        "manifest `[entry].file` does not exist: `{display}` (set a valid path in coil.toml or pass a .hy file)"
                    ),
                );
            }
            display
        }
        None => fail_and_exit(
            pipeline,
            ErrorCode::MissingInputFile,
            "missing input file or command (pass a .hy file, or set `[entry].file` in coil.toml)",
        ),
    }
}

fn print_opt_stats(text: bool, json: bool) {
    if !text && !json {
        return;
    }
    let stats = compiler::last_opt_stats();
    if text {
        eprint!("{}", stats.format_text());
    }
    if json {
        eprintln!("{}", stats.format_json());
    }
}

fn apply_pgo_use(pipeline: &mut Pipeline, path: Option<&str>) {
    let Some(path) = path else {
        return;
    };
    match std::fs::read_to_string(path) {
        Ok(s) => match compiler::ProfileData::from_json(&s) {
            Ok(p) => pipeline.set_pgo_profile(Some(p)),
            Err(compiler::LoadError::Version { found, expected }) => {
                eprintln!(
                    "warning: PGO profile version {found} != {expected}; using heuristics"
                );
                pipeline.set_pgo_profile(None);
            }
            Err(compiler::LoadError::Parse(msg)) => {
                eprintln!("warning: PGO profile parse error ({msg}); using heuristics");
                pipeline.set_pgo_profile(None);
            }
            Err(compiler::LoadError::Io(msg)) => {
                eprintln!("warning: PGO profile io error ({msg}); using heuristics");
                pipeline.set_pgo_profile(None);
            }
        },
        Err(_) => {
            eprintln!("warning: PGO profile `{path}` not found; using heuristics");
        }
    }
}

fn write_pgo_profile(path: Option<&str>) {
    let Some(path) = path else {
        return;
    };
    let snap = machine::pgo::snapshot();
    let json = if snap.function_keys.is_empty()
        && snap.block_counts.is_empty()
        && snap.branch_counts.is_empty()
    {
        compiler::current_profile()
            .unwrap_or_else(compiler::ProfileData::new)
            .to_json()
    } else {
        compiler::profile_from_runtime(
            &snap.function_keys,
            snap.block_counts,
            snap.branch_counts,
        )
        .to_json()
    };
    if let Err(e) = std::fs::write(path, json) {
        eprintln!("warning: failed to write PGO profile `{path}`: {e}");
    }
}

fn compile_to_archive(pipeline: &mut Pipeline, filename: &str, output: &str) {
    // Multi-file entry: discovers `use` / `mod` via coil.toml.
    let (bytecode, constants) = match pipeline.compile_src_from_file(filename) {
        Ok(ok) => ok,
        Err(()) => {
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };

    let debug = pipeline.program_debug();

    let program = ArchivedProgram {
        version: ARCHIVE_VERSION,
        static_slot_count: pipeline.static_slot_count(),
        constants,
        strings: pipeline.strings().to_vec(),
        bytecode,
        source_files: debug.source_files,
        debug_locs: debug.debug_locs,
        fn_symbols: debug.fn_symbols,
    };

    let bytes = match rkyv::to_bytes::<Error>(&program) {
        Ok(b) => b,
        Err(e) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Unable to serialize bytecode archive: {e}"),
        ),
    };

    if let Err(e) = std::fs::write(output, bytes.as_slice()) {
        fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Unable to write compiled output to `{output}`: {e}"),
        );
    }
}

mod archive_staleness {
    use std::path::Path;
    use std::time::SystemTime;

    use common::ProgramDebug;

    pub(super) fn archive_mtime(path: &str) -> Option<SystemTime> {
        std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
    }

    /// Like [`archive_mtime`], but also tries project-root-relative paths.
    pub(super) fn archive_source_mtime(path: &str) -> Option<SystemTime> {
        if let Some(m) = archive_mtime(path) {
            return Some(m);
        }
        let p = Path::new(path);
        if p.is_absolute() {
            return None;
        }
        let Ok(mut dir) = std::env::current_dir() else {
            return None;
        };
        loop {
            let candidate = dir.join(p);
            if let Some(s) = candidate.to_str()
                && let Some(m) = archive_mtime(s)
            {
                return Some(m);
            }
            if dir.join("coil.toml").is_file() {
                break;
            }
            if !dir.pop() {
                break;
            }
        }
        None
    }

    /// True when `path` refers to the same file as `other` (best-effort).
    pub(super) fn same_source_path(path: &str, other: &str) -> bool {
        if path == other {
            return true;
        }
        let a = Path::new(path);
        let b = Path::new(other);
        if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize()) {
            return ca == cb;
        }
        let norm = |s: &str| s.replace('\\', "/").trim_start_matches("./").to_string();
        let a_s = norm(path);
        let b_s = norm(other);
        if a_s == b_s {
            return true;
        }
        fn proper_path_suffix(full: &str, suffix: &str) -> bool {
            if suffix.is_empty() || !suffix.contains('/') {
                return false;
            }
            full.len() > suffix.len()
                && full.ends_with(suffix)
                && full.as_bytes()[full.len() - suffix.len() - 1] == b'/'
        }
        proper_path_suffix(&a_s, &b_s) || proper_path_suffix(&b_s, &a_s)
    }

    /// Whether a cached archive must be rebuilt for `entry`.
    pub(super) fn archive_is_stale(entry: &str, archive: &str, debug: &ProgramDebug) -> bool {
        let Some(arch_mtime) = archive_mtime(archive) else {
            return true;
        };

        if debug.source_files.is_empty() {
            return match archive_source_mtime(entry) {
                Some(src) => src > arch_mtime,
                None => true,
            };
        }

        let entry_known = debug
            .source_files
            .iter()
            .any(|s| same_source_path(s, entry));
        if !entry_known {
            return true;
        }

        for src in &debug.source_files {
            match archive_source_mtime(src) {
                Some(m) if m > arch_mtime => return true,
                None => return true,
                _ => {}
            }
        }

        match archive_source_mtime(entry) {
            Some(src) => src > arch_mtime,
            None => true,
        }
    }

    /// Entry-only mtime compare.
    pub(super) fn source_newer_than_archive(filename: &str, archive: &str) -> bool {
        match (archive_mtime(archive), archive_mtime(filename)) {
            (Some(arch), Some(src)) => src > arch,
            _ => false,
        }
    }
}

/// Canonical entry path for FFI `base_dir` resolution (best-effort absolute).
fn ffi_entry_path(entry: &Path) -> PathBuf {
    std::fs::canonicalize(entry).unwrap_or_else(|_| entry.to_path_buf())
}

/// Warn when a cached `.hyc` is older than sources recorded in its debug bundle.
fn maybe_warn_stale_archive(
    pipeline: &mut Pipeline,
    archive: &str,
    debug: &ProgramDebug,
) {
    if debug.source_files.is_empty() {
        return;
    }
    let entry = debug
        .source_files
        .iter()
        .find(|p| {
            Path::new(p)
                .extension()
                .is_some_and(|ext| ext == "hy")
                && !p.contains("stdlib/")
        })
        .map(|s| s.as_str())
        .unwrap_or(archive);
    if archive_staleness::archive_is_stale(entry, archive, debug) {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!(
                "Bytecode archive `{archive}` may be stale (recorded sources are newer). Recompile with `coil compile … -o {archive}` or run `coil <entry.hy>` directly."
            ),
        );
    }
}

/// Warn when a stale default `out.hyc` exists beside an in-memory run entry.
fn maybe_warn_stale_default_out(pipeline: &mut Pipeline, entry: &str, debug: &ProgramDebug) {
    if !Path::new(DEFAULT_OUT).exists() {
        return;
    }
    if archive_staleness::archive_is_stale(entry, DEFAULT_OUT, debug) {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!(
                "`{DEFAULT_OUT}` is older than sources for `{entry}` and is not used by the default run. Refresh with `coil compile {entry} -o {DEFAULT_OUT}`."
            ),
        );
    }
}

/// Run archived bytecode. Returns `true` when a language-level `panic` aborted.
pub(crate) fn execute_archive(
    pipeline: &Pipeline,
    bytecode: &[Byte],
    constants: &[u64],
    strings: &[String],
    static_slots: u32,
    debug: ProgramDebug,
    entry: Option<&Path>,
    operand_stack_slots: u32,
) -> bool {
    let operand_slots = operand_stack_slots
        .max(machine::DEFAULT_OPERAND_STACK_SLOTS as u32) as usize;
    let entry = entry.map(ffi_entry_path);
    let mut machine = Machine::<256>::with_operand_capacity(operand_slots);
    pipeline.wire_vm_ffi(&mut machine, entry.as_deref());
    pipeline.wire_host_natives(&mut machine);
    pipeline.wire_thread_program(&mut machine, bytecode, constants, strings);
    machine.set_program_debug(debug);
    machine.run_raw(bytecode, constants, strings, static_slots);
    machine.panicked()
}

fn cmd_build_and_run(pipeline: &mut Pipeline, filename: &str, opt_stats: bool, opt_stats_json: bool) {
    let (bytecode, constants) = match pipeline.compile_src_from_file(filename) {
        Ok(ok) => ok,
        Err(()) => {
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };
    print_opt_stats(opt_stats, opt_stats_json);

    let strings = pipeline.strings().to_vec();
    let static_slots = pipeline.static_slot_count();
    let debug = pipeline.program_debug();

    if let Err(e) = pipeline.finish_reporting() {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!("failed to flush diagnostics: {e}"),
        );
        let _ = pipeline.finish_reporting();
    }

    maybe_warn_stale_default_out(pipeline, filename, &debug);
    let entry = ffi_entry_path(Path::new(filename));
    if execute_archive(
        pipeline,
        &bytecode,
        &constants,
        &strings,
        static_slots,
        debug,
        Some(entry.as_path()),
        pipeline.operand_stack_slots(),
    ) {
        exit(1);
    }
}

fn cmd_compile(
    pipeline: &mut Pipeline,
    filename: &str,
    output: &str,
    opt_stats: bool,
    opt_stats_json: bool,
) {
    compile_to_archive(pipeline, filename, output);
    print_opt_stats(opt_stats, opt_stats_json);
    if let Err(e) = pipeline.finish_reporting() {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!("failed to flush diagnostics: {e}"),
        );
        let _ = pipeline.finish_reporting();
    }
}

fn cmd_run(pipeline: &mut Pipeline, archive: &str) {
    let (bytecode, constants, strings, static_slots, debug) = match try_load_archive(archive) {
        Ok(ok) => ok,
        Err(LoadErr::Missing) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Bytecode archive `{archive}` not found"),
        ),
        Err(LoadErr::Corrupt) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Bytecode archive `{archive}` is corrupt"),
        ),
        Err(LoadErr::Version(v)) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!(
                "Bytecode archive version {} is not compatible with runtime {}. Please recompile from source.",
                format_archive_version(v),
                format_archive_version(ARCHIVE_VERSION)
            ),
        ),
    };

    maybe_warn_stale_archive(pipeline, archive, &debug);

    if let Err(e) = pipeline.finish_reporting() {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!("failed to flush diagnostics: {e}"),
        );
        let _ = pipeline.finish_reporting();
    }

    // Weak base_dir: archive parent, for relative FFI dload paths.
    let entry = Path::new(archive);
    if execute_archive(
        pipeline,
        &bytecode,
        &constants,
        &strings,
        static_slots,
        debug,
        Some(entry),
        machine::DEFAULT_OPERAND_STACK_SLOTS as u32,
    ) {
        exit(1);
    }
}

fn collect_test_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("tests directory `{}` not found", dir.display()));
    }

    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("unable to read `{}`: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("unable to read directory entry: {e}"))?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("hy") {
                out.push(path);
            }
        }
        Ok(())
    }
    walk(dir, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "no `.hy` test files found under `{}`",
            dir.display()
        ));
    }
    Ok(files)
}

/// Negative syntax / type tests live under any path segment named `compile_fail`.
/// Those files must fail to compile; a successful compile is a harness failure.
fn is_compile_fail(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "compile_fail")
}

/// Classify a `catch_unwind` compile result for a `compile_fail/` file.
/// Only a clean diagnostic rejection (`Ok(Err(()))`) is harness success.
/// Panic does not count (release builds use `panic = "abort"`).
fn compile_fail_rejected<T>(compiled: &std::thread::Result<Result<T, ()>>) -> bool {
    matches!(compiled, Ok(Err(())))
}

fn run_test_case(
    pipeline: &Pipeline,
    bytecode: &[Byte],
    constants: &[u64],
    strings: &[String],
    entry: Option<&Path>,
    name: &str,
    offset: u32,
) -> bool {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut machine = Machine::<256>::default();
            pipeline.wire_vm_ffi(&mut machine, entry);
            pipeline.wire_host_natives(&mut machine);
            machine.set_program_debug(pipeline.program_debug());
            machine.init_static_slots(pipeline.static_slot_count());
            machine.load_program(bytecode, constants, strings);
            if let Some(main) = pipeline.main_offset() {
                let setup = pipeline.prologue_jmp_target();
                if setup != main {
                    machine.halt_first_jump_to(setup as usize, main);
                    machine.run_from(setup as usize);
                }
            }
            let ret = machine.call_function(offset, &[]);
            !machine.panicked() && machine.result_is_ok(ret)
        }));
    match result {
        Ok(ok) => {
            if !ok {
                eprintln!("> Test \"{name}\" failed");
            }
            ok
        }
        Err(_) => {
            eprintln!("> Test \"{name}\" failed");
            false
        }
    }
}

/// Run the test harness over `root` and return `(passed, failed)` without exiting.
/// Extracted from `cmd_test` so unit tests can assert compile_fail inversion and
/// fail-fast behavior without terminating the process.
fn run_test_suite(
    config: ReportConfig,
    root: &Path,
    fail_fast: bool,
    opt_level: OptLevel,
) -> Result<(usize, usize), String> {
    let files = collect_test_files(root)?;

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut stop = false;

    for path in &files {
        if stop {
            break;
        }
        let display = path.display().to_string();
        let expect_compile_fail = is_compile_fail(path);
        let format = config.format;
        // Expected compile rejection: suppress ariadne noise so the harness
        // summary stays readable when many compile_fail files exist.
        let mut pipeline = if expect_compile_fail {
            Pipeline::with_reporter(config.clone(), Box::new(std::io::sink()))
        } else {
            Pipeline::with_reporter(config.clone(), writer_for(format))
        };
        pipeline.set_include_tests(true);
        pipeline.set_opt_level(opt_level);

        // catch_unwind isolates a compiler ICE from aborting the whole
        // harness under panic=unwind. Release builds use panic=abort, so
        // compile_fail fixtures must reject via Ok(Err(())), not panic.
        let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline.compile_src_from_file(&display)
        }));
        let cases: Vec<(String, u32)> = pipeline.test_cases().to_vec();
        let _ = pipeline.finish_reporting();

        let file_ok = if expect_compile_fail {
            // Only a clean diagnostic rejection counts. A panic is a
            // harness failure (and aborts under release panic=abort).
            if compile_fail_rejected(&compiled) {
                passed += 1;
                true
            } else {
                failed += 1;
                match &compiled {
                    Ok(Ok(_)) => {
                        eprintln!("> Test \"{display}\" failed (expected compile failure)");
                    }
                    Err(_) => {
                        eprintln!("> Test \"{display}\" failed (compiler panicked)");
                    }
                    Ok(Err(())) => unreachable!("compile_fail_rejected is true for Ok(Err)"),
                }
                if fail_fast {
                    stop = true;
                }
                false
            }
        } else {
            match compiled {
                Err(_) => {
                    failed += 1;
                    eprintln!("> Test \"{display}\" failed (compiler panicked)");
                    if fail_fast {
                        stop = true;
                    }
                    false
                }
                Ok(Err(())) => {
                    failed += 1;
                    eprintln!("> Test \"{display}\" failed");
                    if fail_fast {
                        stop = true;
                    }
                    false
                }
                Ok(Ok((bytecode, constants))) => {
                    let strings = pipeline.strings().to_vec();
                    let static_slots = pipeline.static_slot_count();
                    let entry = path.as_path();
                    if cases.is_empty() {
                        // Legacy: whole-file `main` is one opaque case.
                        let debug = pipeline.program_debug();
                        let operand_stack_slots = pipeline.operand_stack_slots();
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            execute_archive(
                                &pipeline,
                                &bytecode,
                                &constants,
                                &strings,
                                static_slots,
                                debug,
                                Some(entry),
                                operand_stack_slots,
                            )
                        }));
                        let ok = match result {
                            Ok(panicked) => !panicked,
                            Err(_) => false,
                        };
                        if ok {
                            passed += 1;
                        } else {
                            failed += 1;
                            eprintln!("> Test \"{display}\" failed");
                            if fail_fast {
                                stop = true;
                            }
                        }
                        ok
                    } else {
                        let mut any_fail = false;
                        for (name, offset) in &cases {
                            let ok = run_test_case(
                                &pipeline,
                                &bytecode,
                                &constants,
                                &strings,
                                Some(entry),
                                name,
                                *offset,
                            );
                            if ok {
                                passed += 1;
                            } else {
                                failed += 1;
                                any_fail = true;
                                if fail_fast {
                                    stop = true;
                                    break;
                                }
                            }
                        }
                        !any_fail
                    }
                }
            }
        };

        if file_ok {
            eprintln!("ok   {display}");
        } else {
            eprintln!("FAILED {display}");
        }
    }

    Ok((passed, failed))
}

fn cmd_test(config: ReportConfig, path: Option<String>, fail_fast: bool, opt_level: OptLevel) {
    let root = path.unwrap_or_else(|| TESTS_DIR.to_string());
    let tests_dir = Path::new(&root);
    let (passed, failed) = match run_test_suite(config.clone(), tests_dir, fail_fast, opt_level) {
        Ok(counts) => counts,
        Err(msg) => {
            let format = config.format;
            let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
            fail_and_exit(&mut pipeline, ErrorCode::IoError, msg);
        }
    };

    eprintln!();
    eprintln!(
        "test result: {}. {passed} passed; {failed} failed; {} total",
        if failed == 0 { "ok" } else { "FAILED" },
        passed + failed
    );

    if failed != 0 {
        exit(1);
    }
}

fn main() {
    let raw_args: Vec<String> = std::env::args().collect();
    let cli = match parse_args(&raw_args) {
        Ok(c) => c,
        Err(msg) => {
            let config = ReportConfig::default();
            let mut pipeline = Pipeline::with_reporter(config, Box::new(std::io::stderr()));
            let code = if msg.contains("mutually")
                || msg.contains("unrecognized")
                || msg.contains("only valid")
                || msg.contains("duplicate")
                || msg.contains("missing path")
            {
                ErrorCode::InvalidCliFlags
            } else {
                ErrorCode::MissingInputFile
            };
            fail_and_exit(&mut pipeline, code, msg);
        }
    };

    if let Command::Version = cli.command {
        print_version();
        exit(0);
    }

    let config = match ReportConfig::from_cli_flags(cli.log_json, cli.log_lsp) {
        Ok(c) => c,
        Err(msg) => {
            let mut pipeline =
                Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::stderr()));
            fail_and_exit(&mut pipeline, ErrorCode::InvalidCliFlags, msg);
        }
    };

    match cli.command {
        Command::Test { path, fail_fast } => cmd_test(config, path, fail_fast, cli.opt_level),
        Command::Dissect { .. } => dispatch_helper("dissect"),
        Command::Debug { .. } => dispatch_helper("debug"),
        Command::Fmt => dispatch_helper("fmt"),
        Command::Lsp => dispatch_helper("lsp"),
        command => {
            let format = config.format;
            let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
            if cli.include_tests {
                pipeline.set_include_tests(true);
            }
            pipeline.set_opt_level(cli.opt_level);
            if cli.opt_stats || cli.opt_stats_json {
                pipeline.set_collect_opt_stats(true);
            }
            apply_pgo_use(&mut pipeline, cli.pgo_use_profile.as_deref());
            if cli.pgo_instrument {
                machine::pgo::reset();
                pipeline.set_pgo_instrument(true);
            }
            match command {
                Command::BuildAndRun { filename } => {
                    let filename = resolve_entry_filename(&mut pipeline, &filename);
                    cmd_build_and_run(
                        &mut pipeline,
                        &filename,
                        cli.opt_stats,
                        cli.opt_stats_json,
                    );
                    write_pgo_profile(cli.pgo_generate_profile.as_deref());
                }
                Command::Compile { filename, output } => {
                    let filename = resolve_entry_filename(&mut pipeline, &filename);
                    cmd_compile(
                        &mut pipeline,
                        &filename,
                        &output,
                        cli.opt_stats,
                        cli.opt_stats_json,
                    );
                    write_pgo_profile(cli.pgo_generate_profile.as_deref());
                }
                Command::Run { archive } => cmd_run(&mut pipeline, &archive),
                Command::Package {
                    filename,
                    output,
                    runner,
                    check_native,
                    strip_debug,
                } => {
                    cmd_package(
                        &mut pipeline,
                        &filename,
                        &output,
                        runner.as_deref(),
                        check_native,
                        strip_debug,
                    );
                    print_opt_stats(cli.opt_stats, cli.opt_stats_json);
                    write_pgo_profile(cli.pgo_generate_profile.as_deref());
                }
                Command::Test { .. }
                | Command::Dissect { .. }
                | Command::Debug { .. }
                | Command::Fmt
                | Command::Lsp
                | Command::Version => {
                    unreachable!()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::archive_staleness::{
        archive_is_stale, archive_mtime, archive_source_mtime, same_source_path,
        source_newer_than_archive,
    };
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
        assert!(err.contains("unrecognized flag"));
        assert!(err.contains("--version"));
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
        // Mutual exclusion is enforced later by ReportConfig::from_cli_flags.
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
        let cli = parse_args(&args(&["--opt-stats", "--opt-stats-json", "compile", "a.hy"])).unwrap();
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

    fn unique_tmp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "coil_cli_{label}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn try_load_archive_missing_corrupt_version_and_ok() {
        let missing = unique_tmp("missing");
        assert!(matches!(
            try_load_archive(missing.to_str().unwrap()),
            Err(LoadErr::Missing)
        ));

        let corrupt = unique_tmp("corrupt");
        std::fs::write(&corrupt, b"not-an-archive").unwrap();
        assert!(matches!(
            try_load_archive(corrupt.to_str().unwrap()),
            Err(LoadErr::Corrupt)
        ));
        let _ = std::fs::remove_file(&corrupt);

        let stale = unique_tmp("stale");
        // Newer minor than this runtime must be rejected.
        let stale_version = common::pack_archive_version(0, 1);
        let bytes = rkyv::to_bytes::<Error>(&ArchivedProgram {
            version: stale_version,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(common::Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![common::DebugLoc::unknown()],
            fn_symbols: Vec::new(),
        })
        .unwrap();
        std::fs::write(&stale, bytes.as_slice()).unwrap();
        let loaded = try_load_archive(stale.to_str().unwrap());
        assert!(
            matches!(loaded, Err(LoadErr::Version(v)) if v == stale_version),
            "{loaded:?}"
        );
        let _ = std::fs::remove_file(&stale);

        let ok_path = unique_tmp("ok");
        let ok_prog = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![42],
            strings: vec![],
            bytecode: vec![Byte::new(common::Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![common::DebugLoc::unknown()],
            fn_symbols: Vec::new(),
        };
        let ok_bytes = rkyv::to_bytes::<Error>(&ok_prog).unwrap();
        std::fs::write(&ok_path, ok_bytes.as_slice()).unwrap();
        let (bc, constants, strings, _, _) =
            try_load_archive(ok_path.to_str().unwrap()).expect("ok archive");
        assert_eq!(constants, vec![42]);
        assert!(strings.is_empty());
        assert_eq!(bc.len(), 1);
        let _ = std::fs::remove_file(&ok_path);
    }

    #[test]
    fn source_newer_than_archive_compares_mtimes() {
        let dir = unique_tmp("mtime");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.hy");
        let arch = dir.join("a.hyc");
        // Archive first, then source after a short sleep so src mtime is newer.
        std::fs::write(&arch, b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&src, b"fn main() {}").unwrap();
        assert!(source_newer_than_archive(
            src.to_str().unwrap(),
            arch.to_str().unwrap()
        ));
        // Missing paths => false
        assert!(!source_newer_than_archive(
            dir.join("nope.hy").to_str().unwrap(),
            arch.to_str().unwrap()
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_when_entry_not_in_source_files() {
        let dir = unique_tmp("stale_entry");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.hy");
        let b = dir.join("b.hy");
        let arch = dir.join("out.hyc");
        std::fs::write(&a, b"fn main() {}").unwrap();
        std::fs::write(&b, b"fn main() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        let debug = ProgramDebug {
            source_files: vec![a.to_string_lossy().into_owned()],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
        };
        // Running b.hy against an archive built from a.hy must rebuild.
        assert!(archive_is_stale(
            b.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        // Same entry, sources not newer => fresh.
        assert!(!archive_is_stale(
            a.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_when_dependency_source_newer() {
        let dir = unique_tmp("stale_dep");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.hy");
        let dep = dir.join("worker.hy");
        let arch = dir.join("out.hyc");
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::fs::write(&dep, b"fn w() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        // Touch only the dependency — entry mtime stays older than the archive.
        std::fs::write(&dep, b"fn w() { /* edited */ }").unwrap();
        let debug = ProgramDebug {
            source_files: vec![
                entry.to_string_lossy().into_owned(),
                dep.to_string_lossy().into_owned(),
            ],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
        };
        assert!(archive_is_stale(
            entry.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_empty_source_files_uses_entry_mtime() {
        let dir = unique_tmp("stale_empty_sources");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.hy");
        let arch = dir.join("out.hyc");
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        let debug = ProgramDebug {
            source_files: vec![],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
        };
        // Entry not newer than archive → fresh via the empty-list branch.
        assert!(!archive_is_stale(
            entry.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&entry, b"fn main() { /* edited */ }").unwrap();
        assert!(archive_is_stale(
            entry.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_when_recorded_source_missing() {
        let dir = unique_tmp("stale_missing_dep");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.hy");
        let arch = dir.join("out.hyc");
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        let missing = dir.join("gone.hy");
        let debug = ProgramDebug {
            source_files: vec![
                entry.to_string_lossy().into_owned(),
                missing.to_string_lossy().into_owned(),
            ],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
        };
        assert!(
            archive_is_stale(entry.to_str().unwrap(), arch.to_str().unwrap(), &debug),
            "missing recorded dependency must invalidate the archive"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_test_files_errors_and_discovers_nested() {
        let missing = unique_tmp("no_tests");
        assert!(collect_test_files(&missing).is_err());

        let empty = unique_tmp("empty_tests");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(collect_test_files(&empty).is_err());

        let root = unique_tmp("nested_tests");
        let nested = root.join("more");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("b.hy"), b"fn main() {}").unwrap();
        std::fs::write(nested.join("a.hy"), b"fn main() {}").unwrap();
        std::fs::write(root.join("ignore.txt"), b"x").unwrap();
        let files = collect_test_files(&root).expect("files");
        assert_eq!(files.len(), 2);
        assert!(files[0].ends_with("a.hy") || files[0].ends_with("b.hy"));
        // Sorted lexicographically by full path.
        assert!(files[0] < files[1]);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn is_compile_fail_detects_path_segment() {
        assert!(is_compile_fail(Path::new("tests/compile_fail/bad.hy")));
        assert!(is_compile_fail(Path::new("/tmp/compile_fail/x.hy")));
        assert!(is_compile_fail(Path::new(
            "suite/nested/compile_fail/deep/x.hy"
        )));
        assert!(!is_compile_fail(Path::new("tests/arithmetic.hy")));
        assert!(!is_compile_fail(Path::new("tests/compile_fail_not/x.hy")));
        assert!(!is_compile_fail(Path::new("tests/my_compile_fail/x.hy")));
    }

    #[test]
    fn compile_fail_rejected_requires_clean_diagnostic_err() {
        let rejected_err: std::thread::Result<Result<(), ()>> = Ok(Err(()));
        assert!(compile_fail_rejected(&rejected_err));

        let unexpected_ok: std::thread::Result<Result<(), ()>> = Ok(Ok(()));
        assert!(!compile_fail_rejected(&unexpected_ok));

        // Panic is NOT a clean rejection (release panic=abort aborts anyway).
        let panicked: std::thread::Result<Result<(), ()>> = Err(Box::new("boom"));
        assert!(!compile_fail_rejected(&panicked));
    }

    #[test]
    fn run_test_suite_compile_fail_inversion_and_mixed_tree() {
        let root = unique_tmp("compile_fail_suite");
        let cf = root.join("compile_fail");
        let pos = root.join("positive");
        std::fs::create_dir_all(&cf).unwrap();
        std::fs::create_dir_all(&pos).unwrap();

        // Type error under compile_fail/ ⇒ harness pass.
        std::fs::write(
            cf.join("bad.hy"),
            "fn main() {\n  let x: int = \"no\";\n}\n",
        )
        .unwrap();
        // Well-typed under compile_fail/ ⇒ harness failure (inverted).
        std::fs::write(
            cf.join("unexpected_ok.hy"),
            "use io::{stdout};
use io::sync::{write_all};\nuse string::{format, to_bytes};\nfn main() {\n  write_all(stdout(), to_bytes(format(\"%i\", 1)));\n}\n",
        )
        .unwrap();
        // Normal positive case still runs.
        std::fs::write(pos.join("ok.hy"), "test(\"ok\") {\n  assert(true)?;\n}\n").unwrap();

        let (passed, failed) =
            run_test_suite(ReportConfig::default(), &root, false, OptLevel::Standard).expect("suite runs");
        assert_eq!(passed, 2, "bad compile_fail + positive ok");
        assert_eq!(failed, 1, "unexpected_ok under compile_fail must fail");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn run_test_suite_fail_fast_stops_after_unexpected_compile_ok() {
        let root = unique_tmp("compile_fail_fail_fast");
        let cf = root.join("compile_fail");
        std::fs::create_dir_all(&cf).unwrap();

        // Lexicographic order: a_ok before z_bad — fail-fast must stop after a_ok.
        std::fs::write(
            cf.join("a_ok.hy"),
            "use io::{stdout};
use io::sync::{write_all};\nuse string::{format, to_bytes};\nfn main() {\n  write_all(stdout(), to_bytes(format(\"%i\", 1)));\n}\n",
        )
        .unwrap();
        std::fs::write(
            cf.join("z_bad.hy"),
            "fn main() {\n  let x: int = \"no\";\n}\n",
        )
        .unwrap();

        let (passed, failed) =
            run_test_suite(ReportConfig::default(), &root, true, OptLevel::Standard).expect("suite runs");
        assert_eq!(failed, 1, "a_ok should fail (unexpected compile success)");
        assert_eq!(passed, 0, "fail-fast must not reach z_bad");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn archive_mtime_returns_none_for_missing() {
        assert!(archive_mtime(unique_tmp("no_mtime").to_str().unwrap()).is_none());
    }

    #[test]
    fn same_source_path_rejects_unrelated_same_basename() {
        assert!(!same_source_path("examples/foo.hy", "vendor/pkg/foo.hy"));
        assert!(!same_source_path("main.hy", "vendor/pkg/main.hy"));
        assert!(same_source_path("examples/foo.hy", "./examples/foo.hy"));
        assert!(same_source_path("src/lib/io.hy", "project/src/lib/io.hy"));
    }

    #[test]
    fn archive_source_mtime_resolves_via_coil_toml_root() {
        let root = unique_tmp("mtime_root");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("coil.toml"), b"[module]\nroots = [\"./src\"]\n").unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("worker.hy");
        std::fs::write(&file, b"fn f() {}").unwrap();

        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&nested).unwrap();
        let m = archive_source_mtime("src/worker.hy");
        std::env::set_current_dir(&prev).unwrap();
        assert!(
            m.is_some(),
            "should resolve src/worker.hy via parent coil.toml root"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Fresh VM per case: soft-fail / panic in earlier cases must not skip later ones.
    #[test]
    fn harness_isolates_cases_and_continues_after_failures() {
        let src = r#"
test("soft fail") {
    assert(false)?;
}
test("panics") {
    panic "boom";
}
test("still runs") {
    assert(true)?;
}
"#;
        let mut pipeline = Pipeline::new();
        pipeline.set_include_tests(true);
        let (bytecode, constants) = pipeline
            .compile_src(src)
            .expect("multi-case harness source should compile");
        let cases = pipeline.test_cases().to_vec();
        assert_eq!(cases.len(), 3, "expected three test(\"…\") cases");
        assert_eq!(cases[0].0, "soft fail");
        assert_eq!(cases[1].0, "panics");
        assert_eq!(cases[2].0, "still runs");

        let mut passed = 0usize;
        let mut failed = 0usize;
        for (name, offset) in &cases {
            if run_test_case(
                &pipeline,
                &bytecode,
                &constants,
                pipeline.strings(),
                None,
                name,
                *offset,
            ) {
                passed += 1;
            } else {
                failed += 1;
            }
        }
        assert_eq!(failed, 2, "soft-fail + panic should each count as failures");
        assert_eq!(
            passed, 1,
            "later case must still run after earlier failures"
        );
    }
}
