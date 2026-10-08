//! `coil-dissect` — in-memory compile + filtered bytecode / IL / AST dump.

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
    about = "In-memory compile and dump filtered bytecode / IL / AST",
    disable_help_subcommand = true,
    after_help = "Pass the entry as a positional `.hy` or `--entry FILE`. A `.hyc` archive\n\
dumps its bytecode instead of compiling."
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
    /// Also print pre-opt stack IL
    #[arg(long)]
    il: bool,
    /// Also print the optimized IL (before fuse / lowering)
    #[arg(long)]
    il_post: bool,
    /// Compile `test` cases too (`__zs_test_N`), like `coil test`
    #[arg(long = "tests")]
    include_tests: bool,
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
        name if name.is_empty() => return Err("dissect requires an entry .hy file".into()),
        name => name,
    };
    let config = ReportConfig::from_cli_flags(cli.log.log_json, cli.log.log_lsp)
        .map_err(|e| e.to_string())?;
    Ok(Some((
        config,
        DissectArgs {
            filename,
            fn_pat: cli.fn_pat,
            show_il: cli.il,
            show_ast: cli.ast,
            show_expand: cli.expand,
            extra_roots: cli.roots.root,
            grants: cli.grants.into_grants(),
            show_mir: cli.mir,
            show_hir: cli.hir,
            show_effects: cli.effects,
            show_il_post: cli.il_post,
            source: !cli.no_source,
            opt_level: cli.opt.level(),
            opt_stats: cli.profile.opt_stats,
            opt_stats_json: cli.profile.opt_stats_json,
            include_tests: cli.include_tests,
        },
    )))
}

fn main() {
    comptime::install();
    let raw: Vec<String> = std::env::args().collect();
    match parse_args(&raw) {
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

    use compiler::HostGrants;

    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("coil-dissect".to_string())
            .chain(parts.iter().map(|s| (*s).to_string()))
            .collect()
    }

    #[test]
    fn parse_host_grants_and_search_paths() {
        let (_cfg, args) = parse_args(&argv(&[
            "a.hy",
            "--allow-attach",
            "--allow-exit",
            "--allow-exec",
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
        assert_eq!(args.filename, "a.hy");
        assert!(args.grants.allow_attach);
        assert!(args.grants.allow_exit);
        assert!(args.grants.allow_exec);
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
    fn parse_default_denies_host_grants() {
        let (_cfg, args) = parse_args(&argv(&["a.hy", "--fn", "main", "--il"]))
            .unwrap()
            .unwrap();
        assert_eq!(args.grants, HostGrants::deny_all());
        assert_eq!(args.fn_pat.as_deref(), Some("main"));
        assert!(args.show_il);
    }

    #[test]
    fn source_stays_on_without_no_source() {
        let (_cfg, args) = parse_args(&argv(&[
            "a.hy",
            "--fn",
            "total",
            "-O",
            "basic",
            "--il-post",
            "--mir",
            "--ast",
        ]))
        .unwrap()
        .unwrap();
        assert!(args.source);
        assert!(args.show_il_post && args.show_mir && args.show_ast);
        assert_eq!(args.opt_level, compiler::OptLevel::Basic);
    }

    #[test]
    fn parse_rejects_missing_dload_stem() {
        assert!(parse_args(&argv(&["a.hy", "--allow-dload"])).is_err());
    }
}
