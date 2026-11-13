//! `coil-dissect` — in-memory compile + filtered MIR / AST dump.
//!
//! A library so that `coil dissect` can run it in process on wasm, where
//! there is no `coil-dissect` to re-exec.

mod dissect;

use std::io::Write;
use std::process::exit;

use clap::{Command, CommandFactory, Parser};
use coil_args::{
    CompileProfileFlags, EntryFlag, HostGrantFlags, LogFlags, OptLevelFlags, RootFlags,
    expand_o_shorts, merge_entry, parse_with, print_cli_error, print_command_help,
};
use compiler::Pipeline;
use dissect::{DissectArgs, cmd_dissect};
use reporting::{ErrorCode, ReportConfig, ReportFormat};

fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
    match format {
        ReportFormat::Pretty => Box::new(std::io::stderr()),
        ReportFormat::Sarif | ReportFormat::Lsp => Box::new(std::io::stdout()),
    }
}

fn fail_and_exit(pipeline: &mut Pipeline, code: ErrorCode, message: impl Into<String>) -> ! {
    pipeline.emit_spanless_error(code, message);
    let _ = pipeline.finish_reporting();
    exit(1);
}

#[derive(Parser, Debug)]
#[command(
    name = "coil-dissect",
    about = "In-memory compile and dump filtered MIR / HIR / AST",
    disable_help_subcommand = true,
    after_help = "Pass the entry as a positional `.coil` or `--entry FILE`. A `.ccoil` archive\n\
dumps its MIR instead of compiling."
)]
struct DissectCli {
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
    /// Compile `test` cases too (`__zs_test_N`), like `coil test`
    #[arg(long = "tests")]
    include_tests: bool,
    /// Also print each body's HIR (typed, desugared tree)
    #[arg(long)]
    hir: bool,
    /// Also print each function's effects and why auto-par left loops sequential
    #[arg(long)]
    effects: bool,
    /// Also print every place lowering boxes a value, with its type and why
    #[arg(long)]
    boxes: bool,
    /// Print the MIR the engines run (the default)
    #[arg(long)]
    mir: bool,
    /// Also print the entry-file AST
    #[arg(long)]
    ast: bool,
    /// Print the entry file after macro expansion (no MIR)
    #[arg(long)]
    expand: bool,
    /// Entry `.coil` file, or a compiled `.ccoil` archive (MIR only)
    #[arg(value_name = "FILE")]
    file: Option<String>,
}

fn command() -> Command {
    let mut command = DissectCli::command();
    command.set_bin_name("coil-dissect");
    command
}

fn parse_args(args: &[String]) -> Result<Option<(ReportConfig, DissectArgs)>, String> {
    let Some(cli) = parse_with::<DissectCli>(command(), &expand_o_shorts(args))? else {
        return Ok(None);
    };
    let filename = match merge_entry(cli.file, cli.entry_flag.entry)? {
        name if name.is_empty() => return Err("dissect requires an entry .coil file".into()),
        name => name,
    };
    let config = ReportConfig::from_cli_flags(cli.log.log_json, cli.log.log_lsp)
        .map_err(|e| e.to_string())?;
    Ok(Some((
        config,
        DissectArgs {
            filename,
            fn_pat: cli.fn_pat,
            show_ast: cli.ast,
            show_expand: cli.expand,
            extra_roots: cli.roots.root,
            grants: cli.grants.into_grants(),
            show_hir: cli.hir,
            show_effects: cli.effects,
            show_boxes: cli.boxes,
            opt_level: cli.opt.level(),
            contracts: cli.opt.contracts,
            fold_calls: cli.opt.fold_calls,
            early_free: cli.opt.early_free,
            auto_par: cli.opt.auto_par,
            opt_stats: cli.profile.opt_stats,
            opt_stats_json: cli.profile.opt_stats_json,
            include_tests: cli.include_tests,
        },
    )))
}

/// Run `coil-dissect` with these arguments (`raw[0]` is the program name).
pub fn run(raw: &[String]) {
    match parse_args(raw) {
        Ok(None) => print_command_help(command(), "coil-dissect"),
        Ok(Some((config, args))) => cmd_dissect(config, args),
        Err(msg) => {
            print_cli_error(&msg);
            exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("coil-dissect".to_string())
            .chain(parts.iter().map(|s| (*s).to_string()))
            .collect()
    }

    #[test]
    fn parse_host_grants_and_search_paths() {
        let (_cfg, args) = parse_args(&argv(&[
            "a.coil",
            "--allow-attach",
            "--allow-exit",
            "--allow-ffi-exec",
            "--allow-dload",
            "tls",
            "--allow-dload=crypto",
            "--ffi-search-path",
            "./native",
            "--ffi-search-path=./more",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(args.filename, "a.coil");
        assert!(args.grants.allow_attach);
        assert!(args.grants.allow_exit);
        assert!(args.grants.allow_ffi_exec);
        assert_eq!(
            args.grants.allow_dload,
            vec!["tls".to_string(), "crypto".to_string()]
        );
        assert_eq!(
            args.grants.ffi_search_paths,
            vec![PathBuf::from("./native"), PathBuf::from("./more")]
        );
    }

    #[test]
    fn parse_rejects_missing_dload_stem() {
        assert!(parse_args(&argv(&["a.coil", "--allow-dload"])).is_err());
    }
}
