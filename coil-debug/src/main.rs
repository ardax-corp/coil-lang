//! `coil-debug` binary entry.

use std::path::PathBuf;
use std::process::exit;

use coil_debug::{DebugArgs, cmd_dap, cmd_debug};
use compiler::HostGrants;
use reporting::ReportConfig;

fn print_help() {
    eprintln!(
        "Usage:\n\
         \x20 coil-debug [--log-json | --log-lsp] [--dap] [<file.hy>] [-x <script>] [--batch]\n\
         \x20            [--allow-attach] [--allow-exit] [--allow-exec] [--allow-ffi-exec]\n\
         \x20            [--allow-dload STEM]... [--ffi-search-path DIR]... [--root DIR]...\n\
         \x20            [--entry FILE]\n\
         \n\
         Options:\n\
         \x20 --dap              Debug Adapter Protocol over stdio (program from DAP launch)\n\
         \x20 -x <script>        Run commands from a script file\n\
         \x20 --batch            Non-interactive; exit after script / stdin\n\
         \x20 --log-json         Emit SARIF 2.1 diagnostics on stdout\n\
         \x20 --log-lsp          Emit LSP Diagnostic NDJSON on stdout\n\
         \x20 --root DIR         Extra module search directory (repeatable; default `src`)\n\
         \x20 --entry FILE       Entry `.hy` (instead of the positional file)\n\
         \x20 --allow-attach     Allow Stream.attach (default deny)\n\
         \x20 --allow-exit       Allow env::exit (default deny)\n\
         \x20 --allow-exec       Allow env::exec (default deny)\n\
         \x20 --allow-ffi-exec   Allow FFI process-exec symbols (default deny)\n\
         \x20 --allow-dload STEM Allow dload of STEM (repeatable; libc still denied)\n\
         \x20 --ffi-search-path  Extra FFI lookup directory (repeatable; not a grant)\n\
         \x20 -h, --help         Show this help"
    );
}

enum Parsed {
    Dap { extra_roots: Vec<PathBuf> },
    Repl(ReportConfig, DebugArgs),
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut log_json = false;
    let mut log_lsp = false;
    let mut batch = false;
    let mut dap = false;
    let mut script: Option<String> = None;
    let mut filename: Option<String> = None;
    let mut grants = HostGrants::deny_all();
    let mut extra_roots: Vec<PathBuf> = Vec::new();
    let mut entry_flag: Option<String> = None;
    let mut i = 1usize;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-h" | "--help" => {
                print_help();
                exit(0);
            }
            "--dap" => dap = true,
            "--log-json" => log_json = true,
            "--log-lsp" => log_lsp = true,
            "--batch" => batch = true,
            "--allow-attach" => grants.allow_attach = true,
            "--allow-exit" => grants.allow_exit = true,
            "--allow-exec" => grants.allow_exec = true,
            "--allow-ffi-exec" => grants.allow_ffi_exec = true,
            "--allow-dload" => {
                i += 1;
                let stem = args
                    .get(i)
                    .ok_or_else(|| "missing STEM after --allow-dload".to_string())?;
                grants.grant_dload_allow(stem.clone());
            }
            s if s.starts_with("--allow-dload=") => {
                grants.grant_dload_allow(s.trim_start_matches("--allow-dload="));
            }
            "--ffi-search-path" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| "missing DIR after --ffi-search-path".to_string())?;
                grants.add_ffi_search_path(PathBuf::from(dir));
            }
            s if s.starts_with("--ffi-search-path=") => {
                grants.add_ffi_search_path(PathBuf::from(s.trim_start_matches("--ffi-search-path=")));
            }
            "--root" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| "missing DIR after --root".to_string())?;
                extra_roots.push(PathBuf::from(dir));
            }
            s if s.starts_with("--root=") => {
                extra_roots.push(PathBuf::from(s.trim_start_matches("--root=")));
            }
            "--entry" => {
                i += 1;
                let path = args
                    .get(i)
                    .ok_or_else(|| "missing FILE after --entry".to_string())?;
                entry_flag = Some(path.clone());
            }
            s if s.starts_with("--entry=") => {
                entry_flag = Some(s.trim_start_matches("--entry=").to_string());
            }
            "-x" => {
                i += 1;
                let path = args
                    .get(i)
                    .ok_or_else(|| "missing path after -x".to_string())?;
                script = Some(path.clone());
            }
            s if s.starts_with('-') => {
                return Err(format!("unrecognized flag `{s}`"));
            }
            _ => {
                if filename.is_some() {
                    return Err("unexpected extra argument".into());
                }
                filename = Some(a.clone());
            }
        }
        i += 1;
    }

    if dap {
        if filename.is_some()
            || script.is_some()
            || batch
            || log_json
            || log_lsp
            || grants != HostGrants::deny_all()
        {
            return Err("--dap cannot be combined with REPL flags or a positional file".into());
        }
        return Ok(Parsed::Dap { extra_roots });
    }

    let filename = match (filename, entry_flag) {
        (Some(a), Some(b)) if a != b => {
            return Err("pass the entry as a positional file or `--entry`, not both".into());
        }
        (Some(a), _) => a,
        (_, Some(b)) => b,
        (None, None) => return Err("debug requires an entry .hy file".into()),
    };
    let config = ReportConfig::from_cli_flags(log_json, log_lsp).map_err(|e| e.to_string())?;
    Ok(Parsed::Repl(
        config,
        DebugArgs {
            filename,
            script,
            batch,
            grants,
            extra_roots,
        },
    ))
}

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    match parse_args(&raw) {
        Ok(Parsed::Dap { extra_roots }) => cmd_dap(extra_roots),
        Ok(Parsed::Repl(config, args)) => cmd_debug(config, args),
        Err(msg) => {
            eprintln!("coil-debug: {msg}");
            print_help();
            exit(1);
        }
    }
}
