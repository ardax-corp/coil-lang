//! `coil verify`: prove contracts with an SMT solver.
//!
//! The entry file is compiled with every contract check on; each function's
//! checks become SMT-LIB queries (`compiler::verify`), and the solver says
//! whether a check can fail. A proved check holds for every input; a
//! counterexample is an input that breaks it.

mod solver;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

use clap::{Command, CommandFactory, Parser};
use coil_args::{EntryFlag, HostGrantFlags, LogFlags, RootFlags, merge_entry, parse_with, print_cli_error, print_command_help};
use compiler::verify::{FnCheck, Goal, ParamShape};
use compiler::{ContractLevel, Pipeline};
use reporting::{ReportConfig, ReportFormat};
use solver::{Answer, Solver, show_value};

#[derive(Parser, Debug)]
#[command(
    name = "coil-verify",
    about = "Prove contracts (requires / ensures / invariant) with an SMT solver",
    disable_help_subcommand = true,
    after_help = "Needs an SMT-LIB solver: `z3` on PATH, or --solver PATH.\n\
Exit status 1 when a contract has a counterexample (or, with --strict, is not proved)."
)]
struct VerifyCli {
    #[command(flatten)]
    log: LogFlags,
    #[command(flatten)]
    grants: HostGrantFlags,
    #[command(flatten)]
    roots: RootFlags,
    #[command(flatten)]
    entry_flag: EntryFlag,
    /// Only functions whose name contains PAT
    #[arg(long = "fn", value_name = "PAT")]
    fn_pat: Option<String>,
    /// The solver binary (default `z3` on PATH, or $COIL_SMT_SOLVER)
    #[arg(long, value_name = "PATH")]
    solver: Option<PathBuf>,
    /// Seconds the solver may spend on one check
    #[arg(long, value_name = "SECS", default_value_t = 10)]
    timeout: u32,
    /// Fail when a check is not proved, not only when it has a counterexample
    #[arg(long)]
    strict: bool,
    /// Also print each SMT-LIB query
    #[arg(long)]
    smt: bool,
    /// Entry `.hy` file
    #[arg(value_name = "FILE")]
    file: Option<String>,
}

fn command() -> Command {
    let mut command = VerifyCli::command();
    command.set_bin_name("coil-verify");
    command
}

fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
    match format {
        ReportFormat::Pretty => Box::new(std::io::stderr()),
        ReportFormat::Sarif | ReportFormat::Lsp => Box::new(std::io::stdout()),
    }
}

#[derive(Debug, Default)]
struct Tally {
    proved: usize,
    failed: usize,
    unknown: usize,
    skipped: usize,
}

fn main() {
    comptime::install();
    let raw: Vec<String> = std::env::args().collect();
    let cli = match parse_with::<VerifyCli>(command(), &raw) {
        Ok(Some(cli)) => cli,
        Ok(None) => {
            print_command_help(command(), "coil-verify");
            exit(0);
        }
        Err(msg) => {
            print_cli_error(&msg);
            exit(1);
        }
    };
    let filename = match merge_entry(cli.file.clone(), cli.entry_flag.entry.clone()) {
        Ok(name) if !name.is_empty() => name,
        Ok(_) => {
            print_cli_error("verify requires an entry .hy file");
            exit(1);
        }
        Err(msg) => {
            print_cli_error(&msg);
            exit(1);
        }
    };
    let config = match ReportConfig::from_cli_flags(cli.log.log_json, cli.log.log_lsp) {
        Ok(c) => c,
        Err(e) => {
            print_cli_error(e);
            exit(1);
        }
    };
    let program = cli.solver.clone().or_else(|| std::env::var_os("COIL_SMT_SOLVER").map(PathBuf::from));
    let solver = Solver::z3(program, cli.timeout);
    if let Err(e) = solver.probe() {
        eprintln!("coil verify: {e}\n  install z3 (https://github.com/Z3Prover/z3) or pass --solver PATH");
        exit(1);
    }

    let checks = match goals_of(config, &cli, &filename) {
        Some(c) => c,
        None => exit(1),
    };
    let source = std::fs::read_to_string(&filename).unwrap_or_default();
    let mut tally = Tally::default();
    let mut out = std::io::stdout().lock();
    for check in &checks {
        if cli.fn_pat.as_deref().is_some_and(|p| !check.name.contains(p)) {
            continue;
        }
        for goal in &check.goals {
            let line = report(&solver, check, goal, &source, &filename, cli.smt, &mut tally);
            let _ = writeln!(out, "{line}");
        }
    }
    let _ = writeln!(
        out,
        "\n{} proved, {} failed, {} not proved, {} skipped",
        tally.proved, tally.failed, tally.unknown, tally.skipped
    );
    if tally.failed > 0 || (cli.strict && tally.unknown > 0) {
        exit(1);
    }
}

/// Compile `filename` and return the goals of its functions.
fn goals_of(config: ReportConfig, cli: &VerifyCli, filename: &str) -> Option<Vec<FnCheck>> {
    let format = config.format;
    let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
    pipeline.set_host_grants(cli.grants.clone().into_grants());
    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    pipeline.bind_project_roots_with_default(dir, cli.roots.root.clone());
    pipeline.set_contracts(ContractLevel::All);
    compiler::verify::start_verify_capture();
    let compiled = pipeline.compile_src_from_file(filename);
    let modules = compiler::verify::take_verify_capture();
    let _ = pipeline.finish_reporting();
    compiled.ok()?;
    let entry = Path::new(filename).canonicalize().ok();
    // The entry compiles as the unnamed module.
    let mine = |module: &str| {
        if module.is_empty() {
            return true;
        }
        let p = Path::new(module);
        p == Path::new(filename) || (entry.is_some() && p.canonicalize().ok() == entry)
    };
    Some(modules.into_iter().filter(|(m, _)| mine(m)).flat_map(|(_, c)| c).collect())
}

fn report(solver: &Solver, check: &FnCheck, goal: &Goal, source: &str, file: &str, smt: bool, tally: &mut Tally) -> String {
    let at = location(source, file, goal.span.0);
    let what = match &goal.callee {
        Some(callee) => format!("{}: call to {callee}: {}", check.name, goal.clause),
        None => format!("{}: {}", check.name, goal.clause),
    };
    if goal.keyword == "decreases" {
        tally.skipped += 1;
        return format!("skipped  {what}  [{at}] (termination is not checked)");
    }
    let mut worst: Option<String> = None;
    let mut not_proved: Option<String> = None;
    for q in &goal.queries {
        if smt {
            println!("; {what}\n{}", q.smt);
        }
        match solver.check(&q.smt) {
            Answer::Unsat => {}
            Answer::Sat(model) => {
                let inputs = show_model(check, &model);
                if q.exact {
                    worst = Some(inputs);
                    break;
                }
                not_proved.get_or_insert_with(|| format!("possible counterexample {inputs}"));
            }
            Answer::Unknown(why) => {
                not_proved.get_or_insert_with(|| format!("solver: {why}"));
            }
        }
    }
    if let Some(inputs) = worst {
        tally.failed += 1;
        return format!("FAILED   {what}  [{at}]\n         counterexample: {inputs}");
    }
    if let Some(why) = not_proved {
        tally.unknown += 1;
        return format!("unknown  {what}  [{at}] ({why})");
    }
    tally.proved += 1;
    format!("proved   {what}  [{at}]")
}

/// `x = 3, len(v) = 0` from a model.
fn show_model(check: &FnCheck, model: &[(String, String)]) -> String {
    let mut parts = Vec::new();
    for p in &check.params {
        let Some(c) = p.consts() else { continue };
        let Some((_, v)) = model.iter().find(|(n, _)| n == c) else { continue };
        let v = show_value(v);
        match p.shape {
            ParamShape::Seq { .. } => parts.push(format!("len({}) = {v}", p.name)),
            _ => parts.push(format!("{} = {v}", p.name)),
        }
    }
    if parts.is_empty() { "(no parameters)".into() } else { parts.join(", ") }
}

fn location(source: &str, file: &str, offset: usize) -> String {
    if offset > source.len() {
        return file.to_string();
    }
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let col = offset - before.rfind('\n').map_or(0, |i| i + 1) + 1;
    format!("{file}:{line}:{col}")
}
