//! `coil-debug` binary entry.

use std::path::PathBuf;
use std::process::exit;

use clap::{Command, CommandFactory, Parser};
use coil_args::{
    EntryFlag, HostGrantFlags, LogFlags, RootFlags, merge_entry, parse_with, print_cli_error,
    print_command_help,
};
use coil_debug::{DebugArgs, cmd_dap, cmd_debug};
use reporting::ReportConfig;

#[derive(Parser, Debug)]
#[command(
    name = "coil-debug",
    about = "GDB-style debugger (REPL; --dap for IDE)",
    disable_help_subcommand = true,
    after_help = "Host grant flags apply to the program under the debugger. With --dap the\n\
program comes from the DAP launch request; launch may also set allowAttach."
)]
struct DebugCli {
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
    /// Debug Adapter Protocol over stdio (program from the DAP launch)
    #[arg(long)]
    dap: bool,
    /// Entry `.hy` file (omit with `--dap`)
    #[arg(value_name = "FILE")]
    file: Option<String>,
}

fn command() -> Command {
    let mut command = DebugCli::command();
    command.set_bin_name("coil-debug");
    command
}

enum Parsed {
    Dap {
        extra_roots: Vec<PathBuf>,
        grants: compiler::HostGrants,
    },
    Repl(ReportConfig, DebugArgs),
}

fn parse_args(args: &[String]) -> Result<Option<Parsed>, String> {
    let Some(cli) = parse_with::<DebugCli>(command(), args)? else {
        return Ok(None);
    };
    if cli.dap {
        if cli.file.is_some() || cli.script.is_some() || cli.batch || cli.log.is_set() {
            return Err("--dap cannot be combined with REPL flags or a positional file".into());
        }
        return Ok(Some(Parsed::Dap {
            extra_roots: cli.roots.root,
            grants: cli.grants.into_grants(),
        }));
    }
    let filename = match merge_entry(cli.file, cli.entry_flag.entry)? {
        name if name.is_empty() => return Err("debug requires an entry .hy file".into()),
        name => name,
    };
    let config = ReportConfig::from_cli_flags(cli.log.log_json, cli.log.log_lsp)
        .map_err(|e| e.to_string())?;
    Ok(Some(Parsed::Repl(
        config,
        DebugArgs {
            filename,
            script: cli.script,
            batch: cli.batch,
            grants: cli.grants.into_grants(),
            extra_roots: cli.roots.root,
        },
    )))
}

fn main() {
    comptime::install();
    let raw: Vec<String> = std::env::args().collect();
    match parse_args(&raw) {
        Ok(None) => print_command_help(command(), "coil-debug"),
        Ok(Some(Parsed::Dap {
            extra_roots,
            grants,
        })) => cmd_dap(extra_roots, grants),
        Ok(Some(Parsed::Repl(config, args))) => cmd_debug(config, args),
        Err(msg) => {
            print_cli_error(&msg);
            exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        std::iter::once("coil-debug".into())
            .chain(parts.iter().map(|s| (*s).to_string()))
            .collect()
    }

    #[test]
    fn parse_dap_allows_host_grants() {
        let parsed = parse_args(&args(&[
            "--dap",
            "--allow-attach",
            "--allow-exit",
            "--root",
            "src",
        ]))
        .expect("dap + grants")
        .expect("not help");
        match parsed {
            Parsed::Dap {
                grants,
                extra_roots,
            } => {
                assert!(grants.allow_attach);
                assert!(grants.allow_exit);
                assert_eq!(extra_roots, vec![PathBuf::from("src")]);
            }
            Parsed::Repl(..) => panic!("expected DAP"),
        }
    }

    #[test]
    fn parse_dap_rejects_positional_and_batch() {
        assert!(parse_args(&args(&["--dap", "a.hy"])).is_err());
        assert!(parse_args(&args(&["--dap", "--batch"])).is_err());
        assert!(parse_args(&args(&["--dap", "-x", "s.txt"])).is_err());
    }

    #[test]
    fn parse_repl_grants() {
        let parsed = parse_args(&args(&["a.hy", "--allow-exec", "--batch"]))
            .expect("repl grants")
            .expect("not help");
        match parsed {
            Parsed::Repl(_, args) => {
                assert!(args.grants.allow_exec);
                assert!(args.batch);
                assert_eq!(args.filename, "a.hy");
            }
            Parsed::Dap { .. } => panic!("expected REPL"),
        }
    }
}
